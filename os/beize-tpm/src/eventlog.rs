//! The firmware's TPM2 event log in the crypto-agile format of the TCG PC
//! Client Platform Firmware Profile, and the replay of a PCR from it.
//!
//! The log is what the firmware says it measured. It is only trustworthy
//! after replaying it gives exactly the value the TPM holds.

use sha2::{Digest, Sha256};

pub const EV_NO_ACTION: u32 = 0x0000_0003;
pub const EV_SEPARATOR: u32 = 0x0000_0004;
pub const EV_EFI_VARIABLE_DRIVER_CONFIG: u32 = 0x8000_0001;
pub const EV_EFI_VARIABLE_AUTHORITY: u32 = 0x8000_00E0;

const TPM_ALG_SHA256: u16 = 0x000B;
const SPEC_ID_SIGNATURE: &[u8; 16] = b"Spec ID Event03\0";

/// One measurement: into which PCR, what kind, its SHA-256 digest and the
/// event data the firmware logged with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub pcr: u32,
    pub event_type: u32,
    pub sha256: [u8; 32],
    pub data: Vec<u8>,
}

/// Parses a binary event log (e.g. `/sys/kernel/security/tpm0/binary_bios_measurements`).
pub fn parse(log: &[u8]) -> Result<Vec<Event>, String> {
    let mut r = Reader::new(log);

    // The first record has the old SHA-1 format and announces the digest
    // algorithms (and their sizes) of all following records.
    let _pcr = r.u32()?;
    let header_type = r.u32()?;
    r.take(20)?;
    let spec = r.sized()?;
    if header_type != EV_NO_ACTION || !spec.starts_with(SPEC_ID_SIGNATURE) {
        return Err("the event log does not start with a Spec ID Event03 header".into());
    }
    let mut spec = Reader::new(&spec[SPEC_ID_SIGNATURE.len()..]);
    spec.take(8)?; // platform class, spec version, errata, uintn size
    let mut sizes = Vec::new();
    for _ in 0..spec.u32()? {
        let alg = spec.u16()?;
        let size = spec.u16()?;
        sizes.push((alg, usize::from(size)));
    }
    if !sizes.iter().any(|&(alg, _)| alg == TPM_ALG_SHA256) {
        return Err("the event log has no SHA-256 digests".into());
    }

    let mut events = Vec::new();
    while !r.is_empty() {
        let pcr = r.u32()?;
        let event_type = r.u32()?;
        let mut sha256 = None;
        for _ in 0..r.u32()? {
            let alg = r.u16()?;
            let size = sizes
                .iter()
                .find(|&&(a, _)| a == alg)
                .map(|&(_, size)| size)
                .ok_or_else(|| format!("unannounced digest algorithm {alg:#06x}"))?;
            let digest = r.take(size)?;
            if alg == TPM_ALG_SHA256 {
                sha256 = Some(digest.try_into().map_err(|_| "bad SHA-256 size")?);
            }
        }
        let sha256 = sha256.ok_or("an event has no SHA-256 digest")?;
        let data = r.sized()?;
        events.push(Event {
            pcr,
            event_type,
            sha256,
            data,
        });
    }
    Ok(events)
}

/// An event's data as text for people: systemd logs UTF-16 descriptions
/// (and tagged events with one inside); anything else as its printable
/// characters. At most 120 characters.
pub fn describe(data: &[u8]) -> String {
    let utf16: String = char::decode_utf16(
        data.as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair)),
    )
    .map(|c| c.unwrap_or('?'))
    .filter(|c| !c.is_control())
    .collect();
    let ascii: String = data
        .iter()
        .map(|&b| b as char)
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .collect();
    // The reading with more readable letters is the right one.
    let readable = |s: &str| s.chars().filter(|c| c.is_ascii_alphanumeric()).count();
    let text = if readable(&utf16) >= readable(&ascii) {
        utf16
    } else {
        ascii
    };
    text.chars().take(120).collect()
}

/// The value `pcr` must have after all logged measurements into it.
pub fn replay(events: &[Event], pcr: u32) -> [u8; 32] {
    events
        .iter()
        .filter(|e| e.pcr == pcr && e.event_type != EV_NO_ACTION)
        .fold([0u8; 32], |value, e| extend(value, e.sha256))
}

/// A PCR's value after extending it with `digest` (SHA-256 bank).
pub fn extend(value: [u8; 32], digest: [u8; 32]) -> [u8; 32] {
    Sha256::new()
        .chain_update(value)
        .chain_update(digest)
        .finalize()
        .into()
}

/// Little-endian reader with bounds checks; the log comes from firmware.
pub struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.at >= self.bytes.len()
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| format!("truncated data at byte {}", self.at))?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    pub fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    /// A u32 length followed by that many bytes.
    fn sized(&mut self) -> Result<Vec<u8>, String> {
        let size = self.u32()? as usize;
        Ok(self.take(size)?.to_vec())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn describes_events_for_people() {
        let utf16: Vec<u8> = "Global credentials initrd\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(describe(&utf16), "Global credentials initrd");
        assert_eq!(describe(b"console=ttyS0\0"), "console=ttyS0");
    }

    /// Builds a crypto-agile log with SHA-256 only.
    pub fn log(events: &[(u32, u32, &[u8])]) -> Vec<u8> {
        let mut spec = SPEC_ID_SIGNATURE.to_vec();
        spec.extend([0u8; 8]);
        spec.extend(1u32.to_le_bytes());
        spec.extend(TPM_ALG_SHA256.to_le_bytes());
        spec.extend(32u16.to_le_bytes());
        spec.push(0);
        let mut out = Vec::new();
        out.extend(0u32.to_le_bytes());
        out.extend(EV_NO_ACTION.to_le_bytes());
        out.extend([0u8; 20]);
        out.extend((spec.len() as u32).to_le_bytes());
        out.extend(spec);
        for &(pcr, event_type, data) in events {
            out.extend(pcr.to_le_bytes());
            out.extend(event_type.to_le_bytes());
            out.extend(1u32.to_le_bytes());
            out.extend(TPM_ALG_SHA256.to_le_bytes());
            out.extend(Sha256::digest(data));
            out.extend((data.len() as u32).to_le_bytes());
            out.extend(data);
        }
        out
    }

    #[test]
    fn parses_and_replays() {
        let events = parse(&log(&[
            (7, EV_SEPARATOR, b"\0\0\0\0"),
            (4, EV_SEPARATOR, b"x"),
        ]))
        .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].pcr, 7);
        let expected: [u8; 32] = Sha256::new()
            .chain_update([0u8; 32])
            .chain_update(Sha256::digest(b"\0\0\0\0"))
            .finalize()
            .into();
        assert_eq!(replay(&events, 7), expected);
        assert_eq!(replay(&events, 9), [0u8; 32]);
    }

    #[test]
    fn rejects_truncated_logs() {
        let mut bytes = log(&[(7, EV_SEPARATOR, b"abcd")]);
        bytes.truncate(bytes.len() - 2);
        assert!(parse(&bytes).is_err());
        assert!(parse(b"garbage").is_err());
    }
}
