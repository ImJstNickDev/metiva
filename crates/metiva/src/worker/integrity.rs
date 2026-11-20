use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, BufReader, Read},
    path::Path,
};

const MAX_EXPANDED: u64 = 64 * 1024 * 1024 * 1024;
const MAX_MEMBERS: usize = 500_000;

fn consume(reader: impl Read, remaining: &mut u64) -> Result<()> {
    let n = io::copy(&mut reader.take(*remaining + 1), &mut io::sink())?;
    ensure!(n <= *remaining, "archive exceeds uncompressed byte limit");
    *remaining -= n;
    Ok(())
}

fn check_tar(reader: impl Read) -> Result<()> {
    let mut archive = tar::Archive::new(reader.take(MAX_EXPANDED + 1));
    let mut remaining = MAX_EXPANDED;
    // Raw iteration streams extension entries too, keeping path and PAX metadata out of memory.
    for (index, entry) in archive.entries()?.raw(true).enumerate() {
        ensure!(index < MAX_MEMBERS, "too many archive members");
        let mut entry = entry?;
        let expected = entry.size();
        let before = remaining;
        consume(&mut entry, &mut remaining)?;
        ensure!(before - remaining == expected, "truncated tar member");
    }
    // The tar reader stops on its first zero block. Require the second terminator
    // and consume the compressed trailer to surface both truncation and CRC errors.
    let mut reader = archive.into_inner();
    let mut end = [0u8; 512];
    reader
        .read_exact(&mut end)
        .context("missing tar terminator")?;
    ensure!(end.iter().all(|b| *b == 0), "invalid tar terminator");
    let mut padding = [0u8; 8192];
    let mut length = 0u64;
    loop {
        let n = reader.read(&mut padding)?;
        if n == 0 {
            break;
        }
        ensure!(
            padding[..n].iter().all(|b| *b == 0),
            "unexpected trailing tar data"
        );
        length += n as u64;
    }
    ensure!(
        reader.limit() > 0 && length.is_multiple_of(512),
        "invalid tar padding or archive exceeds uncompressed byte limit"
    );
    Ok(())
}

pub(crate) fn verify(
    path: &Path,
    filename: &str,
    expected_size: Option<u64>,
    expected_digest: Option<&str>,
    hashes: &std::collections::BTreeMap<String, String>,
) -> Result<(String, u64)> {
    let file = File::open(path)?;
    let size = file.metadata()?.len();
    if let Some(expected) = expected_size {
        ensure!(expected == size, "artifact size mismatch");
    }
    let mut hasher = Sha256::new();
    let mut sha384 = hashes.contains_key("sha384").then(sha2::Sha384::new);
    let mut sha512 = hashes.contains_key("sha512").then(sha2::Sha512::new);
    let mut sha1 = hashes.contains_key("sha1").then(sha1::Sha1::new);
    let mut md5 = hashes.contains_key("md5").then(md5::Md5::new);
    let mut buffer = vec![0u8; 128 * 1024];
    let mut reader = BufReader::new(file);
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        let bytes = &buffer[..n];
        hasher.update(bytes);
        if let Some(h) = &mut sha384 {
            h.update(bytes);
        }
        if let Some(h) = &mut sha512 {
            h.update(bytes);
        }
        if let Some(h) = &mut sha1 {
            h.update(bytes);
        }
        if let Some(h) = &mut md5 {
            h.update(bytes);
        }
    }
    for (alg, observed) in [
        ("sha384", sha384.map(|v| hex::encode(v.finalize()))),
        ("sha512", sha512.map(|v| hex::encode(v.finalize()))),
        ("sha1", sha1.map(|v| hex::encode(v.finalize()))),
        ("md5", md5.map(|v| hex::encode(v.finalize()))),
    ] {
        if let Some(digest) = observed {
            ensure!(hashes[alg] == digest, "artifact {alg} mismatch");
        }
    }
    let digest = hex::encode(hasher.finalize());
    if let Some(expected) = expected_digest {
        ensure!(expected == digest, "artifact SHA-256 mismatch");
    }
    let name = filename.to_ascii_lowercase();
    if name.ends_with(".zip") || name.ends_with(".jar") || name.ends_with(".apk") {
        let mut archive = zip::ZipArchive::new(BufReader::new(File::open(path)?))
            .context("invalid ZIP directory")?;
        ensure!(archive.len() <= MAX_MEMBERS, "too many archive members");
        let mut remaining = MAX_EXPANDED;
        for index in 0..archive.len() {
            let mut member = archive.by_index(index)?;
            ensure!(
                member.size() <= remaining,
                "archive exceeds uncompressed byte limit"
            );
            let before = remaining;
            consume(&mut member, &mut remaining).context("invalid ZIP member")?;
            ensure!(before - remaining == member.size(), "truncated ZIP member");
        }
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        check_tar(flate2::read::MultiGzDecoder::new(BufReader::new(
            File::open(path)?,
        )))?;
    } else if name.ends_with(".gz") {
        consume(
            flate2::read::MultiGzDecoder::new(BufReader::new(File::open(path)?)),
            &mut MAX_EXPANDED.clone(),
        )?;
    } else if name.ends_with(".tar") {
        ensure!(size >= 512, "truncated tar archive");
        check_tar(BufReader::new(File::open(path)?))?;
    }
    Ok((digest, size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, io::Write};
    #[test]
    fn checksum_and_archive_corruption() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"abc").unwrap();
        let mut hashes = BTreeMap::from([
            (
                "md5".to_owned(),
                "900150983cd24fb0d6963f7d28e17f72".to_owned(),
            ),
            (
                "sha512".to_owned(),
                hex::encode(sha2::Sha512::digest(b"abc")),
            ),
        ]);
        assert!(verify(file.path(), "abc.bin", Some(3), None, &hashes).is_ok());
        hashes.insert("md5".into(), "00000000000000000000000000000000".into());
        assert!(verify(file.path(), "abc.bin", None, None, &hashes).is_err());
        assert!(verify(file.path(), "broken.zip", None, None, &BTreeMap::new()).is_err());
        let mut archive = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, "hello", b"abc".as_slice())
            .unwrap();
        let tar = archive.into_inner().unwrap();
        std::fs::write(file.path(), &tar).unwrap();
        assert!(verify(file.path(), "valid.tar", None, None, &BTreeMap::new()).is_ok());
        std::fs::write(file.path(), &tar[..514]).unwrap();
        assert!(verify(file.path(), "truncated.tar", None, None, &BTreeMap::new()).is_err());
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(b"compressed").unwrap();
        let mut data = gz.finish().unwrap();
        data.pop();
        std::fs::write(file.path(), data).unwrap();
        assert!(verify(file.path(), "truncated.gz", None, None, &BTreeMap::new()).is_err());
    }
}
