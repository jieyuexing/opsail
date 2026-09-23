use super::{Error, Result, invalid};
use ring::digest::{SHA256, digest};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Cursor, Read, Write},
    path::Path,
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};
#[derive(Clone, Copy)]
pub struct Limits {
    compressed: u64,
    expanded: u64,
}
impl Limits {
    pub fn new(compressed: Option<u64>, expanded: Option<u64>) -> Result<Self> {
        let x = Self {
            compressed: compressed.unwrap_or(64 * 1024 * 1024),
            expanded: expanded.unwrap_or(256 * 1024 * 1024),
        };
        if x.compressed == 0
            || x.expanded == 0
            || x.compressed > 512 * 1024 * 1024
            || x.expanded > 512 * 1024 * 1024
        {
            return Err(Error::Request("package limits must be 1..512 MiB".into()));
        }
        Ok(x)
    }
}
pub fn sha(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>> {
    let f = File::open(path)?;
    if !f.metadata()?.is_file() || f.metadata()?.len() > max {
        return Err(invalid("source is not a bounded regular file"));
    }
    let mut bytes = Vec::new();
    f.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(invalid("compressed size limit exceeded"));
    }
    Ok(bytes)
}
pub struct Package {
    bytes: Vec<u8>,
    pub parts: BTreeMap<String, Vec<u8>>,
}
impl Package {
    pub fn read(path: &Path, limits: Limits) -> Result<Self> {
        Self::from_bytes(read_bounded(path, limits.compressed)?, limits)
    }
    fn from_bytes(bytes: Vec<u8>, limits: Limits) -> Result<Self> {
        if bytes.len() as u64 > limits.compressed {
            return Err(invalid("compressed size limit exceeded"));
        }
        let mut zip = ZipArchive::new(Cursor::new(&bytes))?;
        if zip.len() > 10000 {
            return Err(invalid("package has more than 10000 parts"));
        }
        let mut parts = BTreeMap::new();
        let mut total = 0u64;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let name = entry.name().to_owned();
            if entry.enclosed_name().is_none()
                || name.contains('\\')
                || name.split('/').any(|p| p == ".." || p == ".")
                || parts.contains_key(&name)
            {
                return Err(invalid("unsafe or duplicate ZIP part"));
            }
            if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
                return Err(invalid("symlink ZIP parts are not supported"));
            }
            total = total
                .checked_add(entry.size())
                .ok_or_else(|| invalid("expanded size overflow"))?;
            if total > limits.expanded {
                return Err(invalid("cumulative expanded size limit exceeded"));
            }
            let mut data = Vec::new();
            (&mut entry)
                .take(limits.expanded + 1)
                .read_to_end(&mut data)?;
            if data.len() as u64 != entry.size() {
                return Err(invalid("ZIP entry length mismatch"));
            }
            parts.insert(name, data);
        }
        drop(zip);
        Ok(Self { bytes, parts })
    }
    pub fn sha(&self) -> String {
        sha(&self.bytes)
    }
    pub fn xml(&self, name: &str) -> Result<super::xml::Document> {
        let bytes = self
            .parts
            .get(name)
            .ok_or_else(|| invalid(format!("missing part: {name}")))?;
        super::xml::parse(
            std::str::from_utf8(bytes).map_err(|_| invalid("non UTF-8 XML is unsupported"))?,
        )
        .map_err(invalid)
    }
    pub fn inventory(&self) -> BTreeMap<String, Value> {
        self.parts
            .iter()
            .map(|(n, b)| (n.clone(), json!({"sha256":sha(b),"expandedBytes":b.len()})))
            .collect()
    }
    pub fn changed_parts(&self, other: &Self) -> Vec<String> {
        self.parts
            .keys()
            .chain(other.parts.keys())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|n| self.parts.get(*n) != other.parts.get(*n))
            .cloned()
            .collect()
    }
    pub fn publish(
        &self,
        source: &Path,
        output: &Path,
        updates: &BTreeMap<String, Vec<u8>>,
        limits: Limits,
    ) -> Result<String> {
        let parent = output
            .parent()
            .ok_or_else(|| invalid("output parent missing"))?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?; // unique, 0600, removed on any error
        let mut source_zip = ZipArchive::new(Cursor::new(&self.bytes))?;
        {
            let mut writer = ZipWriter::new(temp.as_file_mut());
            writer.set_raw_comment(source_zip.comment().to_vec().into_boxed_slice());
            for i in 0..source_zip.len() {
                let entry = source_zip.by_index(i)?;
                if let Some(data) = updates.get(entry.name()) {
                    let mut options =
                        SimpleFileOptions::default().compression_method(entry.compression());
                    if let Some(time) = entry.last_modified() {
                        options = options.last_modified_time(time)
                    }
                    if let Some(mode) = entry.unix_mode() {
                        options = options.unix_permissions(mode)
                    }
                    writer.start_file(entry.name(), options)?;
                    writer.write_all(data)?;
                } else {
                    writer.raw_copy_file(entry)?
                }
            }
            writer.finish()?;
        }
        temp.as_file().sync_all()?;
        let candidate = Self::read(temp.path(), limits)?;
        for (name, data) in &self.parts {
            if candidate.parts.get(name) != Some(updates.get(name).unwrap_or(data)) {
                return Err(invalid("candidate package verification failed"));
            }
        }
        if candidate.parts.len() != self.parts.len() {
            return Err(invalid("candidate part count changed"));
        }
        for name in updates.keys() {
            candidate.xml(name)?;
        }
        // A syntactically valid part is insufficient: verify indexes, style
        // relationships and worksheet structure using the edit loader too.
        super::workbook::Book::load(&candidate)?;
        // Re-read the original just before publishing. Editing operates only on
        // the immutable snapshot; no source bytes are ever written.
        if sha(&read_bounded(source, limits.compressed)?) != self.sha() {
            return Err(Error::Request(
                "source changed during patch; candidate discarded".into(),
            ));
        }
        let hash = candidate.sha();
        temp.persist_noclobber(output)
            .map_err(|e| Error::Io(e.error))?;
        Ok(hash)
    }
}
