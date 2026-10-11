//! JSON: read through libsystemd's sd-json (in the image anyway, and what
//! systemd itself parses user records with); written by hand – a handful of
//! strings, numbers and flags.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::marker::PhantomData;
use std::ptr;

const SD_JSON_PARSE_SENSITIVE: c_int = 1 << 0;

#[link(name = "systemd")]
unsafe extern "C" {
    fn sd_json_parse(
        string: *const c_char,
        flags: c_int,
        ret: *mut *mut c_void,
        line: *mut c_uint,
        column: *mut c_uint,
    ) -> c_int;
    fn sd_json_variant_unref(v: *mut c_void) -> *mut c_void;
    fn sd_json_variant_by_key(v: *mut c_void, key: *const c_char) -> *mut c_void;
    fn sd_json_variant_is_object(v: *mut c_void) -> c_int;
    fn sd_json_variant_is_string(v: *mut c_void) -> c_int;
    fn sd_json_variant_string(v: *mut c_void) -> *const c_char;
    fn sd_json_variant_is_boolean(v: *mut c_void) -> c_int;
    fn sd_json_variant_boolean(v: *mut c_void) -> c_int;
    fn sd_json_variant_is_unsigned(v: *mut c_void) -> c_int;
    fn sd_json_variant_unsigned(v: *mut c_void) -> u64;
    fn sd_json_variant_is_array(v: *mut c_void) -> c_int;
    fn sd_json_variant_elements(v: *mut c_void) -> usize;
    fn sd_json_variant_by_index(v: *mut c_void, index: usize) -> *mut c_void;
}

/// A parsed JSON object, owned.
pub struct Json(*mut c_void);

/// A JSON object inside a [`Json`], borrowed from it.
#[derive(Clone, Copy)]
pub struct Object<'a>(*mut c_void, PhantomData<&'a Json>);

impl Json {
    /// Parses an object. `sensitive`: it carries secrets, which sd-json then
    /// erases from memory when it frees them.
    pub fn parse(text: &str, sensitive: bool) -> Result<Self, String> {
        let text = CString::new(text).map_err(|_| "a NUL byte in JSON")?;
        let mut v = ptr::null_mut();
        let flags = if sensitive {
            SD_JSON_PARSE_SENSITIVE
        } else {
            0
        };
        // SAFETY: NUL-terminated input, valid out pointer; on success the
        // variant is ours until sd_json_variant_unref.
        let r = unsafe {
            sd_json_parse(
                text.as_ptr(),
                flags,
                &mut v,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        let mut bytes = text.into_bytes();
        bytes.fill(0);
        if r < 0 {
            return Err("not valid JSON".into());
        }
        let json = Self(v);
        // SAFETY: a valid variant.
        if unsafe { sd_json_variant_is_object(v) } == 0 {
            return Err("not a JSON object".into());
        }
        Ok(json)
    }

    pub fn object(&self) -> Object<'_> {
        Object(self.0, PhantomData)
    }
}

impl Drop for Json {
    fn drop(&mut self) {
        // SAFETY: the variant parsed in `parse`, not used afterwards.
        unsafe { sd_json_variant_unref(self.0) };
    }
}

impl<'a> Object<'a> {
    fn field(&self, key: &str) -> Option<*mut c_void> {
        let key = CString::new(key).ok()?;
        // SAFETY: a valid object and a NUL-terminated key; the result is
        // borrowed from the object (or null).
        let v = unsafe { sd_json_variant_by_key(self.0, key.as_ptr()) };
        (!v.is_null()).then_some(v)
    }

    /// The string `key`, if it is one.
    pub fn str(&self, key: &str) -> Option<&'a str> {
        let v = self.field(key)?;
        // SAFETY: a valid variant; its string lives as long as the object.
        unsafe {
            if sd_json_variant_is_string(v) == 0 {
                return None;
            }
            CStr::from_ptr(sd_json_variant_string(v)).to_str().ok()
        }
    }

    /// The flag `key`, if it is one.
    pub fn bool(&self, key: &str) -> Option<bool> {
        let v = self.field(key)?;
        // SAFETY: a valid variant.
        unsafe { (sd_json_variant_is_boolean(v) != 0).then(|| sd_json_variant_boolean(v) != 0) }
    }

    /// The unsigned number `key`, if it is one.
    pub fn unsigned(&self, key: &str) -> Option<u64> {
        let v = self.field(key)?;
        // SAFETY: a valid variant.
        unsafe { (sd_json_variant_is_unsigned(v) != 0).then(|| sd_json_variant_unsigned(v)) }
    }

    /// The first element of the array `key`, if it is a string.
    pub fn first_str(&self, key: &str) -> Option<&'a str> {
        let v = self.field(key)?;
        // SAFETY: a valid variant; elements are borrowed from it, their
        // strings live as long as the object.
        unsafe {
            if sd_json_variant_is_array(v) == 0 || sd_json_variant_elements(v) == 0 {
                return None;
            }
            let first = sd_json_variant_by_index(v, 0);
            if first.is_null() || sd_json_variant_is_string(first) == 0 {
                return None;
            }
            CStr::from_ptr(sd_json_variant_string(first)).to_str().ok()
        }
    }

    /// The object `key`, if it is one.
    pub fn object(&self, key: &str) -> Option<Object<'a>> {
        let v = self.field(key)?;
        // SAFETY: a valid variant.
        (unsafe { sd_json_variant_is_object(v) } != 0).then_some(Object(v, PhantomData))
    }
}

/// `text` as a JSON string, quotes included.
pub fn string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_strings() {
        assert_eq!(string("joe"), "\"joe\"");
        assert_eq!(string("a\"b\\c\nd"), "\"a\\\"b\\\\c\\u000ad\"");
        assert_eq!(string("Jörg"), "\"Jörg\"");
    }

    #[test]
    fn reads_what_it_writes() {
        let text = format!(
            "{{\"name\":{},\"admin\":true,\"inner\":{{\"x\":\"y\"}},\"uid\":1000,\
             \"list\":[\"a\",\"b\"]}}",
            string("Jörg \"J\"")
        );
        let json = Json::parse(&text, true).unwrap();
        let object = json.object();
        assert_eq!(object.unsigned("uid"), Some(1000));
        assert_eq!(object.first_str("list"), Some("a"));
        assert_eq!(object.first_str("name"), None, "not an array");
        assert_eq!(object.str("name"), Some("Jörg \"J\""));
        assert_eq!(object.bool("admin"), Some(true));
        assert_eq!(object.object("inner").and_then(|o| o.str("x")), Some("y"));
        assert_eq!(object.str("admin"), None, "not a string");
        assert_eq!(object.str("missing"), None);
        assert!(Json::parse("[1]", false).is_err(), "not an object");
        assert!(Json::parse("{", false).is_err());
    }
}
