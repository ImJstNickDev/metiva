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
    for (index, entry) in archive.entries()?.enumerate() {
        ensure!(index < MAX_MEMBERS, "too many archive members");
        consume(entry?, &mut remaining)?;
    }
    // Read through the compressed trailer as well, so CRC and truncation errors surface.
    let mut reader = archive.into_inner();
    consume(&mut reader, &mut remaining)?;
    ensure!(
        reader.limit() > 0,
        "archive exceeds uncompressed byte limit"
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
