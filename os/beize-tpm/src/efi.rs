//! UEFI structures as they appear in measurements: UEFI_VARIABLE_DATA and
//! EFI_SIGNATURE_LIST (the format of PK, KEK, db and dbx).

use crate::eventlog::Reader;

/// A measured EFI variable: name and contents (UEFI_VARIABLE_DATA).
#[derive(Debug)]
pub struct Variable {
    pub name: String,
    pub data: Vec<u8>,
}

pub fn parse_variable(bytes: &[u8]) -> Result<Variable, String> {
    let mut r = Reader::new(bytes);
    r.take(16)?; // vendor GUID
    let name_chars = usize::try_from(r.u64()?).map_err(|_| "variable name too long")?;
    let data_len = usize::try_from(r.u64()?).map_err(|_| "variable data too long")?;
    let name_bytes = r.take(name_chars.checked_mul(2).ok_or("variable name too long")?)?;
    let name = String::from_utf16(
        &name_bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| u16::from_le_bytes(pair))
            .collect::<Vec<_>>(),
    )
    .map_err(|_| "variable name is not UTF-16")?;
    let data = r.take(data_len)?.to_vec();
    Ok(Variable { name, data })
}

/// One entry of a signature database: its type (X.509, SHA-256, ...) and
/// its data (a certificate or hash). The owner GUID is metadata anyone can
/// set, so it is not part of an entry's identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Entry {
    pub kind: [u8; 16],
    pub data: Vec<u8>,
}

/// All entries of one or more concatenated EFI_SIGNATURE_LISTs.
pub fn signature_entries(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    let mut r = Reader::new(bytes);
    let mut entries = Vec::new();
    while !r.is_empty() {
        let kind: [u8; 16] = r.array()?;
        let list_size = r.u32()? as usize;
        let header_size = r.u32()? as usize;
        let signature_size = r.u32()? as usize;
        let body = list_size
            .checked_sub(28 + header_size)
            .ok_or("signature list smaller than its header")?;
        r.take(header_size)?;
        if signature_size <= 16 || body % signature_size != 0 {
            return Err("malformed signature list".into());
        }
        for _ in 0..body / signature_size {
            let signature = r.take(signature_size)?;
            entries.push(Entry {
                kind,
                data: signature[16..].to_vec(),
            });
        }
    }
    Ok(entries)
}

/// The entry an EV_EFI_VARIABLE_AUTHORITY measurement names: an
/// EFI_SIGNATURE_DATA (owner GUID, then the certificate) without its type.
pub fn authority_data(signature_data: &[u8]) -> Result<&[u8], String> {
    signature_data
        .get(16..)
        .filter(|data| !data.is_empty())
        .ok_or_else(|| "empty authority".into())
}

/// What the firmware hands beize-tpm, for the fuzzer (`fuzz/`, which takes
/// this module and the event log's as they are): an event log, and the EFI
/// variables and signature lists inside its events. Whatever the bytes, the
/// parsers return an error and never panic.
#[cfg(any(test, fuzzing))]
pub fn fuzz_firmware(data: &[u8]) {
    use crate::eventlog;

    let variable = |bytes: &[u8]| {
        if let Ok(var) = parse_variable(bytes) {
            let _ = signature_entries(&var.data);
            let _ = authority_data(&var.data);
        }
    };
    if let Ok(events) = eventlog::parse(data) {
        for event in &events {
            let _ = eventlog::describe(&event.data);
            variable(&event.data);
        }
        let _ = eventlog::replay(&events, 7);
    }
    variable(data);
    let _ = signature_entries(data);
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub const X509: [u8; 16] = [
        0xa1, 0x59, 0xc0, 0xa5, 0xe4, 0x94, 0xa7, 0x4a, 0x87, 0xb5, 0xab, 0x15, 0x5c, 0x2b, 0xf0,
        0x72,
    ];

    /// One EFI_SIGNATURE_LIST with a single entry, as sbsiglist writes it.
    pub fn list(owner: u8, data: &[u8]) -> Vec<u8> {
        let mut out = X509.to_vec();
        out.extend((28 + 16 + data.len() as u32).to_le_bytes());
        out.extend(0u32.to_le_bytes());
        out.extend((16 + data.len() as u32).to_le_bytes());
        out.extend([owner; 16]);
        out.extend(data);
        out
    }

    pub fn variable(name: &str, data: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; 16];
        out.extend((name.encode_utf16().count() as u64).to_le_bytes());
        out.extend((data.len() as u64).to_le_bytes());
        for unit in name.encode_utf16() {
            out.extend(unit.to_le_bytes());
        }
        out.extend(data);
        out
    }

    #[test]
    fn parses_variables_and_lists() {
        let mut db = list(1, b"cert-a");
        db.extend(list(2, b"cert-b"));
        let var = parse_variable(&variable("db", &db)).unwrap();
        assert_eq!(var.name, "db");
        let entries = signature_entries(&var.data).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|e| e.data.as_slice())
                .collect::<Vec<_>>(),
            [b"cert-a".as_slice(), b"cert-b"]
        );
        // The owner is ignored: the same certificate with another owner is the same entry.
        assert_eq!(
            signature_entries(&list(9, b"cert-a")).unwrap()[0],
            entries[0]
        );
    }

    /// Every input the fuzzer keeps (fuzz/corpus, and what once crashed in
    /// fuzz/regressions) runs on every test, without a fuzzer.
    #[test]
    fn fuzz_inputs_still_pass() {
        let fuzz = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz");
        let mut inputs = 0;
        for dir in ["corpus/firmware", "regressions/firmware"] {
            for entry in std::fs::read_dir(fuzz.join(dir)).into_iter().flatten() {
                fuzz_firmware(&std::fs::read(entry.unwrap().path()).unwrap());
                inputs += 1;
            }
        }
        assert!(inputs > 0);
    }

    #[test]
    fn rejects_malformed_lists() {
        let mut bad = list(1, b"cert");
        bad.truncate(bad.len() - 1);
        assert!(signature_entries(&bad).is_err());
        assert!(parse_variable(&[0u8; 10]).is_err());
    }
}
