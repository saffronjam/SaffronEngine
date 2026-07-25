//! The `.smodel` (`SMDL`) container: a 64-byte header, a 32-byte-stride chunk table,
//! and 16-byte-aligned payloads that bundle a model's `.smesh` (MESH), textures
//! (STEX), materials (SMAT), animations (SANM), thumbnail (THMB), and a front-loaded
//! metadata chunk (META) into one file.
//!
//! The header/TOC are reinterpreted with **safe** `bytemuck` over `#[repr(C)]` Pod
//! structs, so the crate's `#![deny(unsafe_code)]` holds.
//!
//! Load-bearing framing rules:
//!
//! - **META front-loading.** The META chunk (if present) is placed first after the TOC
//!   and recorded in `meta_offset`/`meta_length`, so a prefix read reaches the metadata
//!   without scanning payloads. Everything else keeps the caller's order.
//! - **16-byte payload alignment.** Each payload offset is `align16`'d; the TOC starts
//!   at `size_of::<SModelHeader>()` and the first payload at `align16(toc_offset + toc_bytes)`.
//! - **`total_length` vs file size validation** on read, plus the chunk-table-in-bounds
//!   and no-overlap checks (sort the payload ranges by offset, reject if any starts
//!   before the previous ends). These are the silent-corruption guards.
//! - **Lazy chunk reads.** [`ContainerReader`] holds the path + header + TOC and reads a
//!   chunk's `[offset, offset + length)` span from disk on demand.

use std::fs;
use std::path::{Path, PathBuf};

use bytemuck::{Pod, Zeroable};

use crate::error::{Error, Result};

/// Container framing version (the SMDL header layout). Bumped only when the byte
/// framing changes; independent of the metadata-chunk schema version.
pub const CONTAINER_FORMAT_VERSION: u32 = 1;
/// Metadata-chunk schema version, stamped into the header for cheap gating.
pub const METADATA_SCHEMA_VERSION: u32 = 1;

/// The four-byte magic at the head of every `.smodel` container.
const MAGIC: [u8; 4] = *b"SMDL";

/// Packs a four-character chunk tag little-endian into a `u32` (tag[0] in the low byte).
const fn fourcc(tag: &[u8; 4]) -> u32 {
    (tag[0] as u32) | ((tag[1] as u32) << 8) | ((tag[2] as u32) << 16) | ((tag[3] as u32) << 24)
}

/// The kind of a `.smodel` chunk; the discriminant is its on-disk fourcc tag.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkKind {
    /// Front-loaded metadata chunk (`META`).
    Meta = fourcc(b"META"),
    /// The model's mesh image (`MESH`); a standalone `.smesh` byte image.
    Mesh = fourcc(b"MESH"),
    /// A texture payload (`STEX`).
    Texture = fourcc(b"STEX"),
    /// A material payload (`SMAT`).
    Material = fourcc(b"SMAT"),
    /// An animation clip payload (`SANM`); a standalone `.sanim` byte image.
    Animation = fourcc(b"SANM"),
    /// A thumbnail payload (`THMB`).
    Thumbnail = fourcc(b"THMB"),
}

/// One `.smodel` container header (64 bytes, little-endian). Mirrors the `.smesh`
/// header discipline: fixed magic, version gate, 64-bit offsets, file-size validation.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct SModelHeader {
    /// `b"SMDL"`.
    pub magic: [u8; 4],
    /// Framing version; [`CONTAINER_FORMAT_VERSION`].
    pub container_version: u32,
    /// Metadata-chunk schema version.
    pub schema_version: u32,
    /// Reserved framing flags.
    pub flags: u32,
    /// Number of [`TocEntry`] records.
    pub toc_count: u32,
    /// Pad to align the following `u64`s.
    pub reserved0: u32,
    /// Byte offset of the chunk table.
    pub toc_offset: u64,
    /// Byte offset of the META chunk (front-loaded; 0 if absent).
    pub meta_offset: u64,
    /// META chunk byte length (0 if absent).
    pub meta_length: u64,
    /// Total file size, validated on read.
    pub total_length: u64,
    /// Pad to 64 bytes.
    pub reserved: [u32; 2],
}

const _: () = assert!(
    size_of::<SModelHeader>() == 64,
    "SModelHeader must be exactly 64 bytes"
);

/// One chunk-table record (32 bytes, fixed stride); offset/length address the payload.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct TocEntry {
    /// The [`ChunkKind`] value.
    pub fourcc: u32,
    /// Per-chunk flags (colorspace, has-skin, ...).
    pub flags: u32,
    /// Stable sub-asset id (0 for META/THMB).
    pub sub_id: u64,
    /// Absolute byte offset of the payload.
    pub offset: u64,
    /// Payload byte length.
    pub length: u64,
}

const _: () = assert!(
    size_of::<TocEntry>() == 32,
    "TocEntry must stay 32 bytes (the .smodel TOC stride)"
);

/// A chunk to write. The caller owns the bytes; [`write_container`] only frames them.
#[derive(Clone, Copy, Debug)]
pub struct ContainerChunk<'a> {
    /// The chunk's kind (its fourcc tag).
    pub kind: ChunkKind,
    /// Stable sub-asset id (0 for META/THMB).
    pub sub_id: u64,
    /// Per-chunk flags.
    pub flags: u32,
    /// The payload bytes; framed verbatim.
    pub bytes: &'a [u8],
}

/// An opened container: its validated header + chunk table, able to slice any chunk's
/// bytes lazily from disk.
///
/// Holds the path and reads each chunk on demand. It is a plain owned struct, moved to
/// its single owner and dropped at end of scope.
#[derive(Clone, Debug)]
pub struct ContainerReader {
    path: PathBuf,
    header: SModelHeader,
    toc: Vec<TocEntry>,
}

/// Rounds `value` up to the next 16-byte boundary.
const fn align16(value: u64) -> u64 {
    (value + 15) & !15
}

/// Writes all chunks into one `.smodel` file.
///
/// The META chunk (if any) is placed first after the TOC and recorded in
/// `meta_offset`/`meta_length`; every payload offset is 16-byte aligned. The whole
/// container is assembled into one buffer, then written in a single call.
pub fn write_container(path: impl AsRef<Path>, chunks: &[ContainerChunk]) -> Result<()> {
    // META is front-loaded (placed first after the TOC) so a prefix read reaches it
    // without scanning payloads; everything else keeps its caller-given order.
    let mut ordered: Vec<&ContainerChunk> = chunks
        .iter()
        .filter(|c| c.kind == ChunkKind::Meta)
        .collect();
    ordered.extend(chunks.iter().filter(|c| c.kind != ChunkKind::Meta));

    let toc_offset = size_of::<SModelHeader>() as u64;
    let toc_bytes = ordered.len() as u64 * size_of::<TocEntry>() as u64;

    let mut toc = vec![TocEntry::default(); ordered.len()];
    let mut cursor = align16(toc_offset + toc_bytes);
    let mut meta_offset = 0u64;
    let mut meta_length = 0u64;
    for (entry, chunk) in toc.iter_mut().zip(ordered.iter()) {
        cursor = align16(cursor);
        entry.fourcc = chunk.kind as u32;
        entry.flags = chunk.flags;
        entry.sub_id = chunk.sub_id;
        entry.offset = cursor;
        entry.length = chunk.bytes.len() as u64;
        if chunk.kind == ChunkKind::Meta {
            meta_offset = entry.offset;
            meta_length = entry.length;
        }
        cursor += entry.length;
    }
    let total_length = cursor;

    let header = SModelHeader {
        magic: MAGIC,
        container_version: CONTAINER_FORMAT_VERSION,
        schema_version: METADATA_SCHEMA_VERSION,
        flags: 0,
        toc_count: ordered.len() as u32,
        reserved0: 0,
        toc_offset,
        meta_offset,
        meta_length,
        total_length,
        reserved: [0, 0],
    };

    let mut buffer = vec![0u8; total_length as usize];
    buffer[..size_of::<SModelHeader>()].copy_from_slice(bytemuck::bytes_of(&header));
    if !toc.is_empty() {
        let toc_end = toc_offset as usize + toc_bytes as usize;
        buffer[toc_offset as usize..toc_end].copy_from_slice(bytemuck::cast_slice(&toc));
    }
    for (entry, chunk) in toc.iter().zip(ordered.iter()) {
        if !chunk.bytes.is_empty() {
            let start = entry.offset as usize;
            buffer[start..start + chunk.bytes.len()].copy_from_slice(chunk.bytes);
        }
    }

    let path = path.as_ref();
    fs::write(path, &buffer).map_err(|e| Error::Io(format!("'{}': {e}", path.display())))
}

/// Reads and validates only the 64-byte header: magic, version, and `total_length` vs
/// the on-disk file size. Cheap (one open + 64-byte read).
pub fn read_container_header(path: impl AsRef<Path>) -> Result<SModelHeader> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|e| Error::Io(format!("'{}': {e}", path.display())))?;
    let head = bytes
        .get(..size_of::<SModelHeader>())
        .ok_or(Error::Truncated)?;
    let header: &SModelHeader = bytemuck::from_bytes(head);
    if header.magic != MAGIC {
        return Err(Error::BadMagic);
    }
    if header.container_version != CONTAINER_FORMAT_VERSION {
        return Err(Error::UnsupportedVersion(header.container_version));
    }
    if header.total_length != bytes.len() as u64 {
        return Err(Error::BadLayout);
    }
    Ok(*header)
}

/// Reads the full container: the validated header + chunk table, returning a
/// [`ContainerReader`] that can slice chunks lazily.
///
/// Validates the chunk table is in bounds, every payload sits past the header and
/// inside the file, and no two payloads overlap (the ranges are sorted by offset and
/// checked for gaps).
pub fn read_container(path: impl AsRef<Path>) -> Result<ContainerReader> {
    let path = path.as_ref();
    let header = read_container_header(path)?;

    let toc_bytes = u64::from(header.toc_count) * size_of::<TocEntry>() as u64;
    if header.toc_offset < size_of::<SModelHeader>() as u64
        || header.toc_offset + toc_bytes > header.total_length
    {
        return Err(Error::BadLayout);
    }

    let bytes = fs::read(path).map_err(|e| Error::Io(format!("'{}': {e}", path.display())))?;
    let toc_start = header.toc_offset as usize;
    let toc_end = toc_start + toc_bytes as usize;
    let toc_slice = bytes.get(toc_start..toc_end).ok_or(Error::Truncated)?;
    let toc: Vec<TocEntry> = bytemuck::cast_slice::<u8, TocEntry>(toc_slice).to_vec();

    // Bounds + overlap validation: every payload sits past the header and inside the
    // file, and no two payloads cover the same bytes.
    for entry in &toc {
        if entry.length == 0 {
            continue;
        }
        if entry.offset < size_of::<SModelHeader>() as u64
            || entry.offset + entry.length > header.total_length
        {
            return Err(Error::BadLayout);
        }
    }
    let mut ranges: Vec<(u64, u64)> = toc
        .iter()
        .filter(|e| e.length != 0)
        .map(|e| (e.offset, e.length))
        .collect();
    ranges.sort_unstable();
    for window in ranges.windows(2) {
        let (prev_offset, prev_length) = window[0];
        let (offset, _) = window[1];
        if offset < prev_offset + prev_length {
            return Err(Error::BadLayout);
        }
    }

    Ok(ContainerReader {
        path: path.to_path_buf(),
        header,
        toc,
    })
}

impl ContainerReader {
    /// The container file path the reader slices chunks from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The validated container header.
    pub fn header(&self) -> &SModelHeader {
        &self.header
    }

    /// The chunk table.
    pub fn toc(&self) -> &[TocEntry] {
        &self.toc
    }

    /// Reads `[entry.offset, entry.offset + entry.length)` from the file, bounds-checked
    /// against `total_length`.
    pub fn read_chunk(&self, entry: &TocEntry) -> Result<Vec<u8>> {
        if entry.offset + entry.length > self.header.total_length {
            return Err(Error::BadLayout);
        }
        let bytes = fs::read(&self.path)
            .map_err(|e| Error::Io(format!("'{}': {e}", self.path.display())))?;
        let start = entry.offset as usize;
        let end = start + entry.length as usize;
        let slice = bytes.get(start..end).ok_or(Error::Truncated)?;
        Ok(slice.to_vec())
    }

    /// The first TOC entry matching `(kind, sub_id)`, or `None`.
    pub fn find(&self, kind: ChunkKind, sub_id: u64) -> Option<&TocEntry> {
        self.toc
            .iter()
            .find(|e| e.fourcc == kind as u32 && e.sub_id == sub_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `const fn` that only compiles for a `Pod` type — proves the derive held for
    /// the header/TOC structs without naming `unsafe`.
    const fn assert_pod<T: bytemuck::Pod>() {}

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "saffron-smodel-{tag}-{}.smodel",
            std::process::id()
        ))
    }

    #[test]
    fn fourcc_packs_little_endian() {
        // tag[0] in the low byte: "META" == 'M' | 'E'<<8 | 'T'<<16 | 'A'<<24.
        assert_eq!(fourcc(b"META"), u32::from_le_bytes(*b"META"));
        assert_eq!(ChunkKind::Meta as u32, u32::from_le_bytes(*b"META"));
        assert_eq!(ChunkKind::Mesh as u32, u32::from_le_bytes(*b"MESH"));
        assert_eq!(ChunkKind::Texture as u32, u32::from_le_bytes(*b"STEX"));
        assert_eq!(ChunkKind::Material as u32, u32::from_le_bytes(*b"SMAT"));
        assert_eq!(ChunkKind::Animation as u32, u32::from_le_bytes(*b"SANM"));
        assert_eq!(ChunkKind::Thumbnail as u32, u32::from_le_bytes(*b"THMB"));
    }

    #[test]
    fn header_and_toc_are_pod() {
        assert_pod::<SModelHeader>();
        assert_pod::<TocEntry>();
    }

    #[test]
    fn struct_strides_are_pinned() {
        assert_eq!(size_of::<SModelHeader>(), 64);
        assert_eq!(size_of::<TocEntry>(), 32);
    }

    #[test]
    fn align16_rounds_up() {
        assert_eq!(align16(0), 0);
        assert_eq!(align16(1), 16);
        assert_eq!(align16(16), 16);
        assert_eq!(align16(17), 32);
    }

    /// MESH first in caller order so META front-loading is actually exercised, plus
    /// META and STEX.
    #[test]
    fn round_trip_with_meta_front_loaded() {
        let meta_bytes: Vec<u8> = (0..12u8).map(|i| 0xA0u8.wrapping_add(i)).collect();
        let mesh_bytes: Vec<u8> = (0..40u8)
            .map(|i| i.wrapping_mul(3).wrapping_add(1))
            .collect();
        let tex_bytes: Vec<u8> = (0..33u8)
            .map(|i| i.wrapping_mul(7).wrapping_add(2))
            .collect();

        let chunks = [
            ContainerChunk {
                kind: ChunkKind::Mesh,
                sub_id: 111,
                flags: 0,
                bytes: &mesh_bytes,
            },
            ContainerChunk {
                kind: ChunkKind::Meta,
                sub_id: 0,
                flags: 0,
                bytes: &meta_bytes,
            },
            ContainerChunk {
                kind: ChunkKind::Texture,
                sub_id: 222,
                flags: 1,
                bytes: &tex_bytes,
            },
        ];

        let path = temp_path("roundtrip");
        write_container(&path, &chunks).unwrap();
        let reader = read_container(&path).unwrap();

        assert_eq!(reader.toc().len(), 3);
        assert_eq!(reader.header().meta_length, meta_bytes.len() as u64);
        assert_ne!(reader.header().meta_offset, 0);

        let meta_entry = reader.find(ChunkKind::Meta, 0).copied().unwrap();
        let mesh_entry = reader.find(ChunkKind::Mesh, 111).copied().unwrap();
        let tex_entry = reader.find(ChunkKind::Texture, 222).copied().unwrap();

        // META is front-loaded: its payload precedes both other payloads.
        assert!(meta_entry.offset < mesh_entry.offset);
        assert!(meta_entry.offset < tex_entry.offset);
        assert_eq!(reader.header().meta_offset, meta_entry.offset);

        // Payloads are 16-byte aligned; STEX carries its flag.
        assert_eq!(mesh_entry.offset % 16, 0);
        assert_eq!(tex_entry.offset % 16, 0);
        assert_eq!(tex_entry.flags, 1);

        // Every chunk's bytes round-trip lazily.
        assert_eq!(reader.read_chunk(&meta_entry).unwrap(), meta_bytes);
        assert_eq!(reader.read_chunk(&mesh_entry).unwrap(), mesh_bytes);
        assert_eq!(reader.read_chunk(&tex_entry).unwrap(), tex_bytes);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn golden_bytes_header_is_frozen() {
        let meta_bytes = [0u8; 8];
        let mesh_bytes = [0u8; 40];
        let tex_bytes = [0u8; 33];
        let chunks = [
            ContainerChunk {
                kind: ChunkKind::Mesh,
                sub_id: 111,
                flags: 0,
                bytes: &mesh_bytes,
            },
            ContainerChunk {
                kind: ChunkKind::Meta,
                sub_id: 0,
                flags: 0,
                bytes: &meta_bytes,
            },
            ContainerChunk {
                kind: ChunkKind::Texture,
                sub_id: 222,
                flags: 1,
                bytes: &tex_bytes,
            },
        ];

        let path = temp_path("golden");
        write_container(&path, &chunks).unwrap();
        let raw = fs::read(&path).unwrap();

        let header: &SModelHeader = bytemuck::from_bytes(&raw[..64]);
        assert_eq!(&header.magic, b"SMDL");
        assert_eq!(header.container_version, 1);
        assert_eq!(header.schema_version, 1);
        assert_eq!(header.flags, 0);
        assert_eq!(header.toc_count, 3);
        assert_eq!(header.toc_offset, 64);
        assert_eq!(header.reserved0, 0);
        assert_eq!(header.reserved, [0, 0]);

        // Layout: header(64) + 3*TocEntry(32) = 160, align16 -> 160 (already aligned).
        // META(8) at 160, align16(168)=176 MESH(40), align16(216)=224 STEX(33),
        // end 257. total_length is the unaligned end of the last payload.
        assert_eq!(header.meta_offset, 160);
        assert_eq!(header.meta_length, 8);
        assert_eq!(header.total_length, 257);
        assert_eq!(raw.len(), 257);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn empty_chunk_list_writes_header_only() {
        let path = temp_path("empty");
        write_container(&path, &[]).unwrap();
        let reader = read_container(&path).unwrap();
        assert_eq!(reader.toc().len(), 0);
        assert_eq!(reader.header().toc_count, 0);
        assert_eq!(reader.header().total_length, 64);
        assert_eq!(reader.header().meta_offset, 0);
        assert!(reader.find(ChunkKind::Mesh, 0).is_none());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn corrupted_magic_is_rejected() {
        let mesh_bytes = [1u8; 16];
        let chunks = [ContainerChunk {
            kind: ChunkKind::Mesh,
            sub_id: 1,
            flags: 0,
            bytes: &mesh_bytes,
        }];
        let path = temp_path("badmagic");
        write_container(&path, &chunks).unwrap();
        let mut raw = fs::read(&path).unwrap();
        raw[0] = b'X';
        fs::write(&path, &raw).unwrap();
        assert!(matches!(read_container_header(&path), Err(Error::BadMagic)));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn lying_total_length_is_rejected() {
        let mesh_bytes = [1u8; 16];
        let chunks = [ContainerChunk {
            kind: ChunkKind::Mesh,
            sub_id: 1,
            flags: 0,
            bytes: &mesh_bytes,
        }];
        let path = temp_path("badlen");
        write_container(&path, &chunks).unwrap();
        let mut raw = fs::read(&path).unwrap();
        // total_length is the 64-bit field at offset 40 (after the four u32s and the
        // reserved0 pad come toc_offset[24], meta_offset[32], meta_length[40]...). Use
        // bytemuck to flip it instead of a hand-counted offset.
        let wrong = raw.len() as u64 + 4096;
        {
            let header: &mut SModelHeader = bytemuck::from_bytes_mut(&mut raw[..64]);
            header.total_length = wrong;
        }
        fs::write(&path, &raw).unwrap();
        assert!(matches!(
            read_container_header(&path),
            Err(Error::BadLayout)
        ));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn out_of_bounds_toc_is_rejected() {
        let mesh_bytes = [1u8; 16];
        let chunks = [ContainerChunk {
            kind: ChunkKind::Mesh,
            sub_id: 1,
            flags: 0,
            bytes: &mesh_bytes,
        }];
        let path = temp_path("badtoc");
        write_container(&path, &chunks).unwrap();
        let mut raw = fs::read(&path).unwrap();
        {
            let header: &mut SModelHeader = bytemuck::from_bytes_mut(&mut raw[..64]);
            // Claim far more TOC entries than the file holds (without growing the file,
            // so total_length still matches and the header check passes).
            header.toc_count = 4096;
        }
        fs::write(&path, &raw).unwrap();
        assert!(matches!(read_container(&path), Err(Error::BadLayout)));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn overlapping_payloads_are_rejected() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let chunks = [
            ContainerChunk {
                kind: ChunkKind::Mesh,
                sub_id: 1,
                flags: 0,
                bytes: &a,
            },
            ContainerChunk {
                kind: ChunkKind::Texture,
                sub_id: 2,
                flags: 0,
                bytes: &b,
            },
        ];
        let path = temp_path("overlap");
        write_container(&path, &chunks).unwrap();
        let mut raw = fs::read(&path).unwrap();
        // Point the second TOC entry's payload offset back into the first chunk's span,
        // forcing an overlap that the sorted-range check must reject.
        let toc_offset = {
            let header: &SModelHeader = bytemuck::from_bytes(&raw[..64]);
            header.toc_offset as usize
        };
        let first_offset = {
            let entries: &[TocEntry] = bytemuck::cast_slice(&raw[toc_offset..toc_offset + 64]);
            entries[0].offset
        };
        {
            let entries: &mut [TocEntry] =
                bytemuck::cast_slice_mut(&mut raw[toc_offset..toc_offset + 64]);
            entries[1].offset = first_offset;
        }
        fs::write(&path, &raw).unwrap();
        assert!(matches!(read_container(&path), Err(Error::BadLayout)));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_io_error() {
        let path = std::env::temp_dir().join("saffron-smodel-does-not-exist.smodel");
        assert!(matches!(read_container_header(&path), Err(Error::Io(_))));
        assert!(matches!(read_container(&path), Err(Error::Io(_))));
    }
}
