//! `view_image`: an image in the workspace, as something the model can see
//! (P39).
//!
//! # One MCP `image` block, the bytes as they are on disk
//!
//! Measured on both Hosts (toexec `evidence/v3-parity/media-surface/`):
//!
//! ```text
//!                      Claude Code 2.1.273           Codex 0.154.0
//! image block          resized to fit 2000x2000,     input_image, as sent
//!                      recompressed toward 500 KB
//! resource blob        written to a file on the      the whole block as JSON
//! (png or pdf)         Agent Node's disk             text, base64 and all
//! ```
//!
//! So it is an `image` block and never a `resource` one. And ccnm does not
//! scale or convert anything: Claude Code does that itself, an image crate
//! would be the largest dependency in the binary, and what Codex does with
//! a large image happens on the model provider's side where ccnm cannot
//! see it either way.
//!
//! Only PNG, JPEG, GIF and WebP go out, recognised by their first bytes,
//! not the file name. Those are what Claude Code treats as images; any
//! other type it writes to the Agent Node's disk and hands the model a path
//! there, which is both useless in a managed session and a copy of a
//! Runtime file on the machine that holds the credentials.
//!
//! The byte limit is Claude Code's: it refuses base64 over 5 MiB, which is
//! [`MAX_IMAGE_BYTES`] before encoding. A bigger image is refused here with
//! the command that shrinks it, rather than sent and dropped there.

use std::io::Read;
use std::path::Path;

use base64::Engine;
use rmcp::schemars;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::mcp::path;

/// Largest image sent, in bytes on disk. Claude Code 2.1.273's
/// `maxBase64Size` is 5 MiB, and base64 turns 3 bytes into 4.
pub const MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024 / 4 * 3;

/// Arguments of `view_image`.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct ViewImageArgs {
    /// Path of a PNG, JPEG, GIF or WebP file, relative to the workspace root.
    pub path: String,
}

/// An image format both Hosts show the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl Format {
    /// Recognise a format from the first bytes of the file.
    pub fn sniff(head: &[u8]) -> Option<Format> {
        if head.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Format::Png)
        } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Format::Jpeg)
        } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
            Some(Format::Gif)
        } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
            Some(Format::Webp)
        } else {
            None
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            Format::Png => "image/png",
            Format::Jpeg => "image/jpeg",
            Format::Gif => "image/gif",
            Format::Webp => "image/webp",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpeg => "JPEG",
            Format::Gif => "GIF",
            Format::Webp => "WebP",
        }
    }
}

/// An image ready to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// One line about the image, sent as a text block ahead of it.
    pub text: String,
    pub format: Format,
    /// Base64 of the file, as the MCP `image` block carries it.
    pub data: String,
}

/// Read the image at `args.path` under the canonical workspace `root`.
pub fn view_image(root: &Path, args: &ViewImageArgs) -> Result<Image> {
    let target = path::resolve_read(root, &args.path)?;
    let rel = target.rel().to_string();

    // `stat` before `open`, for the reason `read_file` gives: opening a
    // fifo blocks until a writer appears.
    let meta = std::fs::metadata(target.abs())
        .map_err(|e| Error::invalid_args(format!("cannot stat {rel}")).with_source(e))?;
    if meta.is_dir() {
        return Err(Error::invalid_args(format!(
            "{rel} is a directory, not a file"
        )));
    }
    if !meta.is_file() {
        return Err(Error::invalid_args(format!(
            "{rel} is not a regular file (fifo, socket or device); ccnm will not open it"
        )));
    }
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(too_big(&rel, meta.len()));
    }

    let file = std::fs::File::open(target.abs())
        .map_err(|e| Error::invalid_args(format!("cannot open {rel}")).with_source(e))?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    // One byte past the limit, so a file that grew after the stat is
    // caught rather than cut into an image that no longer decodes.
    file.take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::invalid_args(format!("cannot read {rel}")).with_source(e))?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(too_big(&rel, bytes.len() as u64));
    }

    let Some(format) = Format::sniff(&bytes) else {
        return Err(not_an_image(&rel, &bytes));
    };
    Ok(Image {
        text: format!("{rel}: {}, {} bytes", format.name(), bytes.len()),
        format,
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
    })
}

fn too_big(rel: &str, bytes: u64) -> Error {
    Error::invalid_args(format!(
        "{rel} is {bytes} bytes; view_image sends images up to {MAX_IMAGE_BYTES} bytes, the most Claude Code accepts. Make a smaller copy inside the workspace with exec_command, e.g. sips -Z 2000 {rel} --out view-small.png on macOS or convert {rel} -resize 2000x2000 view-small.png with ImageMagick, view that, and delete it afterwards"
    ))
}

/// Name what the file is when that is cheap to tell, because the next step
/// depends on it: SVG is text, and the rest need converting.
fn not_an_image(rel: &str, bytes: &[u8]) -> Error {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
    if head.contains("<svg") {
        return Error::invalid_args(format!(
            "{rel} is an SVG, which is text: read it with read_file"
        ));
    }
    Error::invalid_args(format!(
        "{rel} is not a PNG, JPEG, GIF or WebP image (checked by its first bytes, not its name); other formats have to be converted with exec_command first"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use ccnm_testdir::TestDir;
    use std::fs;

    /// A valid 2x2 red PNG, as `zlib` and the PNG spec make it.
    const PNG_2X2: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x02, 0x00, 0x00, 0x00, 0xFD,
        0xD4, 0x9A, 0x73, 0x00, 0x00, 0x00, 0x10, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x44, 0x0C, 0x10, 0x0A, 0x00, 0x1F, 0xEE, 0x03, 0xFD, 0x8B, 0x5F, 0x14,
        0xD4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn workspace(name: &str) -> TestDir {
        let dir = std::env::temp_dir().join(format!("ccnm-image-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("shots")).unwrap();
        TestDir::adopt(fs::canonicalize(&dir).unwrap())
    }

    fn view(root: &Path, path: &str) -> Result<Image> {
        view_image(
            root,
            &ViewImageArgs {
                path: path.to_string(),
            },
        )
    }

    #[test]
    fn an_image_comes_back_whole_as_base64_with_its_type() {
        let root = workspace("png");
        fs::write(root.join("shots/red.png"), PNG_2X2).unwrap();
        let image = view(&root, "shots/red.png").unwrap();
        assert_eq!(image.format, Format::Png);
        assert_eq!(image.format.mime_type(), "image/png");
        assert_eq!(
            image.text,
            format!("shots/red.png: PNG, {} bytes", PNG_2X2.len())
        );
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&image.data)
            .unwrap();
        assert_eq!(decoded, PNG_2X2, "sent as it is on disk, nothing converted");
    }

    #[test]
    fn the_format_is_read_from_the_bytes_not_the_name() {
        assert_eq!(Format::sniff(PNG_2X2), Some(Format::Png));
        assert_eq!(Format::sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Format::Jpeg));
        assert_eq!(Format::sniff(b"GIF89a\x01\x00"), Some(Format::Gif));
        assert_eq!(Format::sniff(b"GIF87a\x01\x00"), Some(Format::Gif));
        assert_eq!(
            Format::sniff(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            Some(Format::Webp)
        );
        assert_eq!(
            Format::sniff(b"RIFF\x24\x00\x00\x00WAVEfmt "),
            None,
            "a WAV is RIFF too"
        );
        assert_eq!(Format::sniff(b"BM\x36\x00"), None, "BMP");
        assert_eq!(Format::sniff(b""), None);

        let root = workspace("names");
        fs::write(root.join("shots/really-a-png.jpg"), PNG_2X2).unwrap();
        assert_eq!(
            view(&root, "shots/really-a-png.jpg").unwrap().format,
            Format::Png
        );
        fs::write(root.join("shots/fake.png"), "not an image\n").unwrap();
        let e = view(&root, "shots/fake.png").unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(e.message().contains("not a PNG, JPEG, GIF or WebP"), "{e}");
    }

    #[test]
    fn an_svg_is_pointed_at_read_file() {
        let root = workspace("svg");
        fs::write(
            root.join("shots/logo.svg"),
            "<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>\n",
        )
        .unwrap();
        let e = view(&root, "shots/logo.svg").unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(e.message().contains("read_file"), "{e}");
    }

    #[test]
    fn an_image_over_the_limit_is_refused_with_the_way_to_shrink_it() {
        let root = workspace("big");
        let mut big = PNG_2X2.to_vec();
        big.resize(MAX_IMAGE_BYTES as usize + 1, 0);
        fs::write(root.join("shots/huge.png"), &big).unwrap();
        let e = view(&root, "shots/huge.png").unwrap_err();
        assert_eq!(e.code(), ErrorCode::InvalidArgs);
        assert!(e.message().contains(&MAX_IMAGE_BYTES.to_string()), "{e}");
        assert!(e.message().contains("exec_command"), "{e}");

        // Exactly at the limit still goes.
        big.truncate(MAX_IMAGE_BYTES as usize);
        fs::write(root.join("shots/edge.png"), &big).unwrap();
        assert_eq!(view(&root, "shots/edge.png").unwrap().format, Format::Png);
        assert_eq!(MAX_IMAGE_BYTES, 3_932_160, "Claude Code's 5 MiB of base64");
    }

    #[test]
    fn the_read_path_policy_applies() {
        let root = workspace("policy");
        for (path, code) in [
            ("../x.png", ErrorCode::Policy),
            ("/etc/hosts", ErrorCode::Policy),
            ("shots/missing.png", ErrorCode::InvalidArgs),
            ("shots", ErrorCode::InvalidArgs),
        ] {
            let e = view(&root, path).unwrap_err();
            assert_eq!(e.code(), code, "{path} -> {e}");
        }
        // Nothing about the machine leaks through an error.
        let e = view(&root, "shots").unwrap_err();
        assert!(!e.message().contains(&root.display().to_string()), "{e}");
    }
}
