//! Shared, bounded attachment snapshots for HTTP and ACP prompts.
use std::path::Path;
use base64::Engine;
use agent_client_protocol::schema::v1::{ContentBlock, ImageContent, TextContent};

pub const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Attachment {
    pub name: String,
    pub content: String,
    pub mime: Option<String>,
}

impl Attachment {
    pub fn read(path: &Path) -> Result<Self, String> {
        use std::io::Read;
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("Attach files, not directories.".into());
        }
        let mut bytes = Vec::new();
        file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_BYTES { return Err("Attachments must be smaller than 8 MiB.".into()); }
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") { Some("image/png") }
            else if bytes.starts_with(b"\xff\xd8\xff") { Some("image/jpeg") }
            else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") { Some("image/gif") }
            else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") { Some("image/webp") }
            else { None };
        let content = if mime.is_some() { base64::engine::general_purpose::STANDARD.encode(bytes) }
            else { String::from_utf8(bytes).map_err(|_| "Use a text/code file or PNG, JPEG, GIF, WebP image.".to_string())? };
        Ok(Self { name: path.display().to_string(), content, mime: mime.map(str::to_string) })
    }

    pub fn context_text(&self) -> String {
        format!("File reference: {}\n<file_content>\n{}\n</file_content>", self.name, self.content)
    }

    pub fn acp(&self) -> ContentBlock {
        match &self.mime {
            Some(mime) => ContentBlock::Image(ImageContent::new(self.content.clone(), mime.clone())),
            None => ContentBlock::Text(TextContent::new(self.context_text())),
        }
    }

    pub fn http(&self) -> serde_json::Value {
        match &self.mime {
            Some(mime) => serde_json::json!({"type":"image_url", "image_url":{"url":format!("data:{mime};base64,{}", self.content)}}),
            None => serde_json::json!({"type":"text", "text":self.context_text()}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attachments_use_provider_content_blocks() {
        let image = Attachment { name: "test.png".into(), content: "aGVsbG8=".into(), mime: Some("image/png".into()) };
        assert!(matches!(image.acp(), ContentBlock::Image(_)));
        assert_eq!(image.http()["image_url"]["url"], "data:image/png;base64,aGVsbG8=");
        let code = Attachment { name: "src/main.rs (selection)".into(), content: "fn main() {}".into(), mime: None };
        assert!(code.http()["text"].as_str().unwrap().contains("fn main() {}"));
        assert!(matches!(code.acp(), ContentBlock::Text(_)));
    }
}
