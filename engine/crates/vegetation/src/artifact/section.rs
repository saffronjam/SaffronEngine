//! Section storage: the raw-or-Zstd choice, the pinned frame settings that keep artifact bytes
//! reproducible, and the streaming validation that hashes a decoded section without buffering it.

use std::borrow::Cow;
use std::io::{Read, Write};

use super::ArtifactDecodeLimits;
use super::container::{RawDescriptor, validate_section_size};
use crate::{ArtifactSectionCodec, ContentHash, Error, Result, VegetationContentHasher};

pub(super) const ZSTD_COMPRESSION_LEVEL: i32 = 10;
pub(super) const ZSTD_WINDOW_LOG: u32 = 27;

pub(super) fn stream_region(
    source: &mut impl Read,
    mut remaining: u64,
    require_zero: bool,
    payload_hasher: &mut VegetationContentHasher,
    artifact_hasher: &mut VegetationContentHasher,
    format: &'static str,
) -> Result<()> {
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let length = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| Error::NumericOverflow)?;
        read_exact_artifact(source, &mut buffer[..length], format)?;
        let bytes = &buffer[..length];
        if require_zero && bytes.iter().any(|byte| *byte != 0) {
            return Err(Error::ArtifactFormat {
                format,
                field: "alignmentPadding".to_owned(),
            });
        }
        payload_hasher.update(bytes)?;
        artifact_hasher.update(bytes)?;
        remaining = remaining
            .checked_sub(u64::try_from(length).map_err(|_| Error::NumericOverflow)?)
            .ok_or(Error::NumericOverflow)?;
    }
    Ok(())
}

pub(super) struct ArtifactHashingReader<'a, R> {
    pub(super) source: R,
    pub(super) payload_hasher: &'a mut VegetationContentHasher,
    pub(super) artifact_hasher: &'a mut VegetationContentHasher,
}

impl<R: Read> Read for ArtifactHashingReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let length = self.source.read(buffer)?;
        self.payload_hasher
            .update(&buffer[..length])
            .and_then(|()| self.artifact_hasher.update(&buffer[..length]))
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(length)
    }
}

struct PrefixReplayReader<R> {
    prefix: [u8; 18],
    prefix_length: usize,
    prefix_position: usize,
    source: R,
}

impl<R: Read> Read for PrefixReplayReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.prefix_position < self.prefix_length {
            let length = buffer.len().min(self.prefix_length - self.prefix_position);
            buffer[..length]
                .copy_from_slice(&self.prefix[self.prefix_position..self.prefix_position + length]);
            self.prefix_position += length;
            return Ok(length);
        }
        self.source.read(buffer)
    }
}

pub(super) fn hash_decoded_reader(
    reader: &mut impl Read,
    expected_size: u64,
) -> std::io::Result<(u64, ContentHash)> {
    let mut hasher = VegetationContentHasher::new();
    let mut decoded_size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let length = reader.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        decoded_size = decoded_size
            .checked_add(
                u64::try_from(length).map_err(|error| std::io::Error::other(error.to_string()))?,
            )
            .filter(|size| *size <= expected_size)
            .ok_or_else(|| std::io::Error::other("decoded section exceeds declared size"))?;
        hasher
            .update(&buffer[..length])
            .map_err(|error| std::io::Error::other(error.to_string()))?;
    }
    let hash = hasher
        .finalize()
        .map(ContentHash::new)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok((decoded_size, hash))
}

pub(super) fn validate_decoded_section<R: Read>(
    mut source: R,
    descriptor: RawDescriptor,
    format: &'static str,
) -> Result<()> {
    let (decoded_size, content_hash) = match descriptor.codec {
        ArtifactSectionCodec::Raw => hash_decoded_reader(&mut source, descriptor.decoded_size)
            .map_err(|source| Error::ArtifactIo { format, source })?,
        ArtifactSectionCodec::Zstd => {
            let prefix_length = usize::try_from(descriptor.stored_size.min(18))
                .map_err(|_| Error::NumericOverflow)?;
            let mut prefix = [0_u8; 18];
            source
                .read_exact(&mut prefix[..prefix_length])
                .map_err(|source| Error::ArtifactCodec {
                    format,
                    section: descriptor.kind,
                    source,
                })?;
            let frame_content_size =
                zstd::zstd_safe::get_frame_content_size(&prefix[..prefix_length])
                    .map_err(|source| Error::ArtifactCodec {
                        format,
                        section: descriptor.kind,
                        source: std::io::Error::other(source.to_string()),
                    })?
                    .ok_or_else(|| Error::ArtifactFormat {
                        format,
                        field: format!("section{}.zstdContentSize", descriptor.kind),
                    })?;
            if frame_content_size != descriptor.decoded_size {
                return Err(Error::ArtifactFormat {
                    format,
                    field: format!("section{}.decodedSize", descriptor.kind),
                });
            }
            let replay = PrefixReplayReader {
                prefix,
                prefix_length,
                prefix_position: 0,
                source,
            };
            let mut decoder = zstd::stream::read::Decoder::new(replay)
                .map_err(|source| Error::ArtifactCodec {
                    format,
                    section: descriptor.kind,
                    source,
                })?
                .single_frame();
            decoder
                .window_log_max(ZSTD_WINDOW_LOG)
                .map_err(|source| Error::ArtifactCodec {
                    format,
                    section: descriptor.kind,
                    source,
                })?;
            let decoded =
                hash_decoded_reader(&mut decoder, descriptor.decoded_size).map_err(|source| {
                    Error::ArtifactCodec {
                        format,
                        section: descriptor.kind,
                        source,
                    }
                })?;
            let buffered = decoder.finish();
            if !buffered.buffer().is_empty() {
                return Err(Error::ArtifactFormat {
                    format,
                    field: format!("section{}.zstdFrameSize", descriptor.kind),
                });
            }
            let mut replay = buffered.into_inner();
            let mut trailing = [0_u8; 1];
            if replay
                .read(&mut trailing)
                .map_err(|source| Error::ArtifactCodec {
                    format,
                    section: descriptor.kind,
                    source,
                })?
                != 0
            {
                return Err(Error::ArtifactFormat {
                    format,
                    field: format!("section{}.zstdFrameSize", descriptor.kind),
                });
            }
            decoded
        }
    };
    if decoded_size != descriptor.decoded_size {
        return Err(Error::ArtifactFormat {
            format,
            field: format!("section{}.decodedSize", descriptor.kind),
        });
    }
    if content_hash != descriptor.content_hash {
        return Err(Error::ArtifactHashMismatch {
            format,
            subject: format!("section {}", descriptor.kind),
        });
    }
    Ok(())
}

pub(super) fn read_exact_artifact(
    source: &mut impl Read,
    bytes: &mut [u8],
    format: &'static str,
) -> Result<()> {
    source.read_exact(bytes).map_err(|source| {
        if source.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::ArtifactTruncated { format }
        } else {
            Error::ArtifactIo { format, source }
        }
    })
}

pub(super) fn encode_section(
    format: &'static str,
    section: u16,
    decoded: &[u8],
) -> Result<(ArtifactSectionCodec, Vec<u8>)> {
    let decoded_size = u64::try_from(decoded.len()).map_err(|_| Error::NumericOverflow)?;

    let mut encoder =
        zstd::stream::Encoder::new(Vec::new(), ZSTD_COMPRESSION_LEVEL).map_err(|source| {
            Error::ArtifactCodec {
                format,
                section,
                source,
            }
        })?;
    encoder
        .include_checksum(true)
        .and_then(|()| encoder.include_dictid(false))
        .and_then(|()| encoder.include_contentsize(true))
        .and_then(|()| encoder.long_distance_matching(false))
        .and_then(|()| encoder.window_log(ZSTD_WINDOW_LOG))
        .and_then(|()| encoder.set_pledged_src_size(Some(decoded_size)))
        .and_then(|()| encoder.write_all(decoded))
        .map_err(|source| Error::ArtifactCodec {
            format,
            section,
            source,
        })?;
    let compressed = encoder.finish().map_err(|source| Error::ArtifactCodec {
        format,
        section,
        source,
    })?;
    if compressed.len() < decoded.len() {
        return Ok((ArtifactSectionCodec::Zstd, compressed));
    }

    let mut raw = Vec::new();
    raw.try_reserve_exact(decoded.len())
        .map_err(|source| Error::MemoryReservation {
            resource: "vegetation artifact raw section",
            source,
        })?;
    raw.extend_from_slice(decoded);
    Ok((ArtifactSectionCodec::Raw, raw))
}

pub(super) fn decode_stored_section<'a>(
    stored: &'a [u8],
    descriptor: RawDescriptor,
    format: &'static str,
    limits: ArtifactDecodeLimits,
) -> Result<Cow<'a, [u8]>> {
    validate_section_size(
        format,
        descriptor.kind,
        "stored",
        descriptor.stored_size,
        limits.max_stored_section_bytes,
    )?;
    validate_section_size(
        format,
        descriptor.kind,
        "decoded",
        descriptor.decoded_size,
        limits.max_decoded_section_bytes,
    )?;
    let decoded = match descriptor.codec {
        ArtifactSectionCodec::Raw => Cow::Borrowed(stored),
        ArtifactSectionCodec::Zstd => {
            let frame_size =
                zstd::zstd_safe::find_frame_compressed_size(stored).map_err(|code| {
                    Error::ArtifactCodec {
                        format,
                        section: descriptor.kind,
                        source: std::io::Error::other(zstd::zstd_safe::get_error_name(code)),
                    }
                })?;
            if frame_size != stored.len() {
                return Err(Error::ArtifactFormat {
                    format,
                    field: format!("section{}.zstdFrameSize", descriptor.kind),
                });
            }
            let frame_content_size = zstd::zstd_safe::get_frame_content_size(stored)
                .map_err(|source| Error::ArtifactCodec {
                    format,
                    section: descriptor.kind,
                    source: std::io::Error::other(source.to_string()),
                })?
                .ok_or_else(|| Error::ArtifactFormat {
                    format,
                    field: format!("section{}.zstdContentSize", descriptor.kind),
                })?;
            if frame_content_size != descriptor.decoded_size {
                return Err(Error::ArtifactFormat {
                    format,
                    field: format!("section{}.decodedSize", descriptor.kind),
                });
            }
            let decoded_size =
                usize::try_from(descriptor.decoded_size).map_err(|_| Error::NumericOverflow)?;
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(decoded_size)
                .map_err(|source| Error::MemoryReservation {
                    resource: "vegetation artifact decoded section",
                    source,
                })?;
            bytes.resize(decoded_size, 0);
            let mut decoder = zstd::stream::read::Decoder::new(stored).map_err(|source| {
                Error::ArtifactCodec {
                    format,
                    section: descriptor.kind,
                    source,
                }
            })?;
            decoder
                .window_log_max(ZSTD_WINDOW_LOG)
                .and_then(|()| decoder.read_exact(&mut bytes))
                .map_err(|source| Error::ArtifactCodec {
                    format,
                    section: descriptor.kind,
                    source,
                })?;
            let mut extra = [0_u8; 1];
            if decoder
                .read(&mut extra)
                .map_err(|source| Error::ArtifactCodec {
                    format,
                    section: descriptor.kind,
                    source,
                })?
                != 0
            {
                return Err(Error::ArtifactFormat {
                    format,
                    field: format!("section{}.decodedSize", descriptor.kind),
                });
            }
            Cow::Owned(bytes)
        }
    };
    if u64::try_from(decoded.len()).map_err(|_| Error::NumericOverflow)? != descriptor.decoded_size
    {
        return Err(Error::ArtifactFormat {
            format,
            field: format!("section{}.decodedSize", descriptor.kind),
        });
    }
    if ContentHash::of(decoded.as_ref()) != descriptor.content_hash {
        return Err(Error::ArtifactHashMismatch {
            format,
            subject: format!("section {}", descriptor.kind),
        });
    }
    Ok(decoded)
}
