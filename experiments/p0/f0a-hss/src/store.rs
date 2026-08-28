use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use crc32fast::hash as crc32;
use serde::Serialize;
use sha2::{Digest, Sha256};

const FILE_MAGIC: &[u8; 8] = b"JMCF0A01";
const BLOCK_MAGIC: &[u8; 4] = b"BLK1";
const FILE_HEADER_BYTES: u64 = 32;
const BLOCK_HEADER_BYTES: u64 = 24;

pub(crate) struct CandidateWriter {
    file: File,
    bytes_written: u64,
    blocks_written: u64,
}

impl CandidateWriter {
    /// Creates a new append-only candidate capture file.
    pub(crate) fn create(
        path: &Path,
        record_stride: u32,
        address: u32,
        rate_hz: u32,
        hss_flags: u32,
        timestamp_frequency_hz: u32,
    ) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .with_context(|| {
                format!(
                    "candidate capture already exists or cannot be created: {}",
                    path.display()
                )
            })?;
        file.write_all(FILE_MAGIC)?;
        file.write_all(&1_u32.to_le_bytes())?;
        file.write_all(&record_stride.to_le_bytes())?;
        file.write_all(&address.to_le_bytes())?;
        file.write_all(&rate_hz.to_le_bytes())?;
        file.write_all(&hss_flags.to_le_bytes())?;
        file.write_all(&timestamp_frequency_hz.to_le_bytes())?;
        Ok(Self {
            file,
            bytes_written: FILE_HEADER_BYTES,
            blocks_written: 0,
        })
    }

    /// Appends one independently checksummed payload block.
    pub(crate) fn append(
        &mut self,
        host_elapsed_us: u64,
        phase: u32,
        payload: &[u8],
    ) -> Result<()> {
        ensure!(
            payload.len() <= 16 * 1024 * 1024,
            "candidate block exceeds 16 MiB"
        );
        self.file.write_all(BLOCK_MAGIC)?;
        self.file
            .write_all(&u32::try_from(payload.len())?.to_le_bytes())?;
        self.file.write_all(&crc32(payload).to_le_bytes())?;
        self.file.write_all(&host_elapsed_us.to_le_bytes())?;
        self.file.write_all(&phase.to_le_bytes())?;
        self.file.write_all(payload)?;
        self.bytes_written += BLOCK_HEADER_BYTES + u64::try_from(payload.len())?;
        self.blocks_written += 1;
        Ok(())
    }

    /// Flushes all candidate evidence to stable storage.
    pub(crate) fn finish(mut self) -> Result<(u64, u64)> {
        self.file.flush()?;
        self.file.sync_all()?;
        Ok((self.bytes_written, self.blocks_written))
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ScanSummary {
    pub(crate) record_stride: u32,
    pub(crate) address: u32,
    pub(crate) rate_hz: u32,
    pub(crate) hss_flags: u32,
    pub(crate) timestamp_frequency_hz: u32,
    pub(crate) valid_blocks: u64,
    pub(crate) valid_payload_bytes: u64,
    pub(crate) truncated_tail: bool,
    pub(crate) crc_error: bool,
}

/// Scans committed blocks and stops before an incomplete or corrupt tail.
pub(crate) fn scan(path: &Path) -> Result<ScanSummary> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut header = [0_u8; FILE_HEADER_BYTES as usize];
    file.read_exact(&mut header)?;
    ensure!(
        &header[..8] == FILE_MAGIC,
        "candidate capture magic mismatch"
    );
    ensure!(
        u32::from_le_bytes(header[8..12].try_into()?) == 1,
        "candidate capture version mismatch"
    );

    let mut summary = ScanSummary {
        record_stride: u32::from_le_bytes(header[12..16].try_into()?),
        address: u32::from_le_bytes(header[16..20].try_into()?),
        rate_hz: u32::from_le_bytes(header[20..24].try_into()?),
        hss_flags: u32::from_le_bytes(header[24..28].try_into()?),
        timestamp_frequency_hz: u32::from_le_bytes(header[28..32].try_into()?),
        valid_blocks: 0,
        valid_payload_bytes: 0,
        truncated_tail: false,
        crc_error: false,
    };
    loop {
        let mut block_header = [0_u8; BLOCK_HEADER_BYTES as usize];
        match file.read_exact(&mut block_header) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => {
                let position = file.stream_position()?;
                summary.truncated_tail = position
                    > FILE_HEADER_BYTES
                        + summary.valid_blocks * BLOCK_HEADER_BYTES
                        + summary.valid_payload_bytes;
                return Ok(summary);
            }
            Err(error) => return Err(error.into()),
        }
        if &block_header[..4] != BLOCK_MAGIC {
            summary.crc_error = true;
            return Ok(summary);
        }
        let payload_len = u32::from_le_bytes(block_header[4..8].try_into()?) as usize;
        if payload_len > 16 * 1024 * 1024 {
            summary.crc_error = true;
            return Ok(summary);
        }
        let expected_crc = u32::from_le_bytes(block_header[8..12].try_into()?);
        let mut payload = vec![0_u8; payload_len];
        if let Err(error) = file.read_exact(&mut payload) {
            if error.kind() == ErrorKind::UnexpectedEof {
                summary.truncated_tail = true;
                return Ok(summary);
            }
            return Err(error.into());
        }
        if crc32(&payload) != expected_crc {
            summary.crc_error = true;
            return Ok(summary);
        }
        summary.valid_blocks += 1;
        summary.valid_payload_bytes += u64::try_from(payload_len)?;
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BenchmarkSummary {
    pub(crate) output: PathBuf,
    pub(crate) partial_output: PathBuf,
    pub(crate) duration_s: u32,
    pub(crate) rate_hz: u32,
    pub(crate) sample_payload_bytes: u32,
    pub(crate) record_stride_bytes: u32,
    pub(crate) sample_count: u64,
    pub(crate) file_bytes: u64,
    pub(crate) write_elapsed_ms: u128,
    pub(crate) throughput_mib_s: f64,
    pub(crate) sha256: String,
    pub(crate) complete_scan: ScanSummary,
    pub(crate) partial_scan: ScanSummary,
    pub(crate) recovery_validated: bool,
}

/// Writes and verifies a deterministic 300-second-equivalent capture.
pub(crate) fn benchmark(
    output: &Path,
    duration_s: u32,
    rate_hz: u32,
    sample_payload_bytes: u32,
) -> Result<BenchmarkSummary> {
    ensure!(
        duration_s > 0 && rate_hz > 0 && sample_payload_bytes > 0,
        "benchmark values must be positive"
    );
    let record_stride = 4_u32
        .checked_add(sample_payload_bytes)
        .context("record stride overflow")?;
    let sample_count = u64::from(duration_s)
        .checked_mul(u64::from(rate_hz))
        .context("sample count overflow")?;
    let mut writer = CandidateWriter::create(output, record_stride, 0, rate_hz, 0, 1000)?;
    let records_per_block = (64 * 1024_u32 / record_stride).max(1);
    let started = Instant::now();
    let mut written = 0_u64;
    while written < sample_count {
        let count = u64::from(records_per_block).min(sample_count - written);
        let mut payload = Vec::with_capacity(usize::try_from(count * u64::from(record_stride))?);
        for index in 0..count {
            let sample = written + index;
            let timestamp_ms = u32::try_from(sample.saturating_mul(1000) / u64::from(rate_hz))?;
            payload.extend_from_slice(&timestamp_ms.to_le_bytes());
            for byte in 0..sample_payload_bytes {
                payload.push((sample as u8).wrapping_add(byte as u8));
            }
        }
        writer.append(
            written.saturating_mul(1_000_000) / u64::from(rate_hz),
            0,
            &payload,
        )?;
        written += count;
    }
    let (file_bytes, _) = writer.finish()?;
    let write_elapsed = started.elapsed();
    let complete_scan = scan(output)?;
    ensure!(
        !complete_scan.truncated_tail && !complete_scan.crc_error,
        "complete candidate file did not verify"
    );

    let partial_output = output.with_extension("partial");
    if partial_output.exists() {
        bail!(
            "partial benchmark output already exists: {}",
            partial_output.display()
        );
    }
    fs::copy(output, &partial_output)?;
    let partial = OpenOptions::new().write(true).open(&partial_output)?;
    partial.set_len(file_bytes.saturating_sub(7))?;
    partial.sync_all()?;
    let partial_scan = scan(&partial_output)?;
    let recovery_validated = partial_scan.truncated_tail
        && !partial_scan.crc_error
        && partial_scan.valid_blocks < complete_scan.valid_blocks;
    ensure!(
        recovery_validated,
        "partial-tail recovery was not demonstrated"
    );

    let elapsed_seconds = write_elapsed.as_secs_f64().max(f64::EPSILON);
    Ok(BenchmarkSummary {
        output: output.to_path_buf(),
        partial_output,
        duration_s,
        rate_hz,
        sample_payload_bytes,
        record_stride_bytes: record_stride,
        sample_count,
        file_bytes,
        write_elapsed_ms: write_elapsed.as_millis(),
        throughput_mib_s: file_bytes as f64 / 1024.0 / 1024.0 / elapsed_seconds,
        sha256: sha256_file(output)?,
        complete_scan,
        partial_scan,
        recovery_validated,
    })
}

/// Computes the SHA-256 identity of one evidence file.
pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
