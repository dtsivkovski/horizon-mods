//! Decides whether a native mod's owner update stays inside the subset that may merge without a
//! reviewer. It reads the whole new package and the previous one as data: nothing is extracted,
//! compiled or run. See README.md for the rules.

pub mod manifests;
pub mod package;
pub mod report;
pub mod source;

pub use report::{Finding, Report, Rule, Side};
use sha2::{Digest, Sha256};

/// One owner update: the registry entry's id and new version, both packages and their pinned
/// SHA-256s, and each `horizon` commit with an SDK release, with the horizon-mixins revision it
/// builds (`None` when that commit names none).
pub struct Update<'a> {
    pub id: &'a str,
    pub version: &'a str,
    pub old: &'a [u8],
    pub old_sha256: &'a str,
    pub new: &'a [u8],
    pub new_sha256: &'a str,
    pub pins: &'a [(String, Option<String>)],
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn check(update: &Update) -> Report {
    let mut report = Report::default();
    let mut hashed = true;
    for (side, bytes, want) in [
        (Side::Old, update.old, update.old_sha256),
        (Side::New, update.new, update.new_sha256),
    ] {
        if sha256_hex(bytes) != want {
            report.add(Rule::Hash, side, "", 0, "SHA-256 differs from the entry");
            hashed = false;
        }
    }
    // Only bytes the entry pins are read.
    if !hashed {
        return report;
    }
    let old = package::read(&mut report, Side::Old, update.old);
    let Some(new) = package::read(&mut report, Side::New, update.new) else {
        return report;
    };
    package::check_layout(&mut report, &new);
    if let Some(old) = &old {
        manifests::check(&mut report, old, &new, update);
    }
    source::check(&mut report, &new);
    report
}

#[cfg(test)]
pub(crate) mod testutil {
    //! Zips written byte by byte, so a test can make the ones no writer would: links, clashing
    //! names, lying headers.
    use std::io::{Cursor, Write};

    pub struct Entry {
        name: Vec<u8>,
        data: Vec<u8>,
        mode: u32,
        method: u16,
        size: Option<u64>,
        crc: Option<u32>,
        uncompressed: u64,
        real_crc: u32,
        flags: u16,
        extra: Vec<u8>,
        local_extra: Vec<u8>,
        local_name: Option<Vec<u8>>,
        local_crc: Option<u32>,
        local_method: Option<u16>,
    }

    impl Entry {
        pub fn raw(name: Vec<u8>, data: &[u8], mode: u32) -> Entry {
            Entry {
                name,
                data: data.to_vec(),
                mode,
                method: 0,
                size: None,
                crc: None,
                uncompressed: data.len() as u64,
                real_crc: crc32(data),
                flags: 0x0800,
                extra: Vec::new(),
                local_extra: Vec::new(),
                local_name: None,
                local_crc: None,
                local_method: None,
            }
        }
        pub fn file(name: &str, data: &[u8]) -> Entry {
            Entry::raw(name.as_bytes().to_vec(), data, 0o100644)
        }
        pub fn mode(name: &str, data: &[u8], mode: u32) -> Entry {
            Entry::raw(name.as_bytes().to_vec(), data, mode)
        }
        pub fn sized(name: &str, data: &[u8], size: u64) -> Entry {
            Entry {
                size: Some(size),
                ..Entry::file(name, data)
            }
        }
        pub fn crc(name: &str, data: &[u8], crc: u32) -> Entry {
            Entry {
                crc: Some(crc),
                ..Entry::file(name, data)
            }
        }
        /// Deflated by the zip crate's writer, then copied out of its archive.
        pub fn deflated(name: &str, data: &[u8]) -> Entry {
            let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .large_file(true);
            w.start_file("x", options).unwrap();
            w.write_all(data).unwrap();
            let bytes = w.finish().unwrap().into_inner();
            let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
            let f = archive.by_index_raw(0).unwrap();
            let start = f.data_start().unwrap() as usize;
            let compressed = bytes[start..start + f.compressed_size() as usize].to_vec();
            Entry {
                data: compressed,
                method: 8,
                uncompressed: data.len() as u64,
                real_crc: crc32(data),
                ..Entry::file(name, b"")
            }
        }
        /// Fields that make the local header or the extra fields say something else.
        pub fn with(self, f: impl FnOnce(&mut Entry)) -> Entry {
            let mut e = self;
            f(&mut e);
            e
        }
        pub fn set_flags(&mut self, flags: u16) {
            self.flags = flags;
        }
        pub fn set_local_name(&mut self, name: &str) {
            self.local_name = Some(name.as_bytes().to_vec());
        }
        pub fn set_local_crc(&mut self, crc: u32) {
            self.local_crc = Some(crc);
        }
        pub fn set_local_method(&mut self, method: u16) {
            self.local_method = Some(method);
        }
        /// A 0x7075 Unicode path field naming `path`, in the central record, the local one or both.
        pub fn unicode_path(&mut self, path: &str, central: bool, local: bool) {
            let mut field = 0x7075u16.to_le_bytes().to_vec();
            field.extend(((5 + path.len()) as u16).to_le_bytes());
            field.push(1);
            field.extend(crc32(&self.name).to_le_bytes());
            field.extend(path.as_bytes());
            if central {
                self.extra.extend(&field);
            }
            if local {
                self.local_extra.extend(&field);
            }
        }
        pub fn raw_extra(&mut self, field: &[u8]) {
            self.extra.extend(field);
            self.local_extra.extend(field);
        }
        pub fn claiming(self, size: u64) -> Entry {
            Entry {
                size: Some(size),
                ..self
            }
        }
    }

    pub fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// A zip of `entries`, in order, each name as given, made on unix with its mode.
    pub fn zip(entries: &[Entry]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for e in entries {
            let offset = out.len() as u32;
            let size = e.size.unwrap_or(e.uncompressed).min(u64::from(u32::MAX)) as u32;
            let crc = e.crc.unwrap_or(e.real_crc);
            let common = |v: &mut Vec<u8>, method: u16, crc: u32, name: &[u8], extra: &[u8]| {
                v.extend(e.flags.to_le_bytes());
                v.extend(method.to_le_bytes());
                v.extend(0u16.to_le_bytes());
                v.extend(0x21u16.to_le_bytes());
                v.extend(crc.to_le_bytes());
                v.extend((e.data.len() as u32).to_le_bytes());
                v.extend(size.to_le_bytes());
                v.extend((name.len() as u16).to_le_bytes());
                v.extend((extra.len() as u16).to_le_bytes());
            };
            let local_name = e.local_name.as_deref().unwrap_or(&e.name);
            out.extend(0x0403_4b50u32.to_le_bytes());
            out.extend(20u16.to_le_bytes());
            let local_method = e.local_method.unwrap_or(e.method);
            common(
                &mut out,
                local_method,
                e.local_crc.unwrap_or(crc),
                local_name,
                &e.local_extra,
            );
            out.extend(local_name);
            out.extend(&e.local_extra);
            out.extend(&e.data);
            central.extend(0x0201_4b50u32.to_le_bytes());
            central.extend((3u16 << 8 | 20).to_le_bytes());
            central.extend(20u16.to_le_bytes());
            common(&mut central, e.method, crc, &e.name, &e.extra);
            central.extend(0u16.to_le_bytes());
            central.extend(0u16.to_le_bytes());
            central.extend(0u16.to_le_bytes());
            central.extend((e.mode << 16).to_le_bytes());
            central.extend(offset.to_le_bytes());
            central.extend(&e.name);
            central.extend(&e.extra);
        }
        let start = out.len() as u32;
        out.extend(&central);
        out.extend(0x0605_4b50u32.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((central.len() as u32).to_le_bytes());
        out.extend(start.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out
    }
}
