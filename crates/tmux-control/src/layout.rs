use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LayoutAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayoutNode {
    pub width: u16,
    pub height: u16,
    pub left: u16,
    pub top: u16,
    pub pane_index: Option<u32>,
    pub axis: Option<LayoutAxis>,
    pub children: Vec<LayoutNode>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LayoutParseError {
    #[error("invalid tmux layout at byte {0}")]
    Invalid(usize),
    #[error("tmux layout has trailing input at byte {0}")]
    Trailing(usize),
    #[error("invalid tmux JSON layout: {0}")]
    Json(String),
}

/// Parses a tmux layout in either form tmux prints.
///
/// tmux 3.8 prints `#{window_layout}` as a versioned JSON document
/// (`{"V":2,"L":{...}}`); earlier releases, and 3.8's own `%layout-change`
/// notifications, print the checksummed `b25d,160x48,0,0{...}` form. A
/// checksummed layout starts with hex digits, so a leading `{` is unambiguous.
pub fn parse_layout(layout: &str) -> Result<LayoutNode, LayoutParseError> {
    if layout.starts_with('{') {
        return parse_json_layout(layout);
    }
    let bytes = layout.as_bytes();
    let start = layout.find(',').ok_or(LayoutParseError::Invalid(0))? + 1;
    let mut parser = Parser {
        bytes,
        offset: start,
    };
    let node = parser.node()?;
    if parser.offset != bytes.len() {
        return Err(LayoutParseError::Trailing(parser.offset));
    }
    Ok(node)
}

#[derive(Deserialize)]
struct JsonLayout {
    #[serde(rename = "V")]
    version: u32,
    #[serde(rename = "L")]
    root: JsonLayoutNode,
}

/// One cell of a version 2 layout. Fields this parser has no use for — the
/// pane's index (`i`), active and last-active flags, a floating pane's
/// z-order — are ignored.
#[derive(Deserialize)]
struct JsonLayoutNode {
    t: String,
    w: u16,
    h: u16,
    x: u16,
    y: u16,
    #[serde(default)]
    c: Vec<JsonLayoutNode>,
    /// The pane id (`%N`): what the checksummed form's leaf number is.
    #[serde(rename = "I")]
    pane_id: Option<String>,
}

fn parse_json_layout(layout: &str) -> Result<LayoutNode, LayoutParseError> {
    let document: JsonLayout =
        serde_json::from_str(layout).map_err(|error| LayoutParseError::Json(error.to_string()))?;
    if document.version != 2 {
        return Err(LayoutParseError::Json(format!(
            "unsupported layout version {}",
            document.version
        )));
    }
    json_node(document.root)
}

fn json_node(node: JsonLayoutNode) -> Result<LayoutNode, LayoutParseError> {
    let invalid = |reason: &str| LayoutParseError::Json(reason.to_owned());
    let mut result = LayoutNode {
        width: node.w,
        height: node.h,
        left: node.x,
        top: node.y,
        pane_index: None,
        axis: None,
        children: Vec::new(),
    };
    match node.t.as_str() {
        "p" => {
            if !node.c.is_empty() {
                return Err(invalid("a pane cell has children"));
            }
            let pane_id = node
                .pane_id
                .ok_or_else(|| invalid("a pane cell has no id"))?;
            result.pane_index = Some(
                pane_id
                    .strip_prefix('%')
                    .and_then(|number| number.parse().ok())
                    .ok_or_else(|| invalid("a pane cell has a malformed id"))?,
            );
        }
        axis @ ("h" | "v") => {
            if node.c.is_empty() {
                return Err(invalid("a split cell has no children"));
            }
            result.axis = Some(if axis == "h" {
                LayoutAxis::Horizontal
            } else {
                LayoutAxis::Vertical
            });
            result.children = node
                .c
                .into_iter()
                .map(json_node)
                .collect::<Result<_, _>>()?;
        }
        _ => return Err(invalid("unknown cell type")),
    }
    Ok(result)
}

struct Parser<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Parser<'_> {
    fn node(&mut self) -> Result<LayoutNode, LayoutParseError> {
        let width = self.number_u16()?;
        self.expect(b'x')?;
        let height = self.number_u16()?;
        self.expect(b',')?;
        let left = self.number_u16()?;
        self.expect(b',')?;
        let top = self.number_u16()?;

        let mut result = LayoutNode {
            width,
            height,
            left,
            top,
            pane_index: None,
            axis: None,
            children: Vec::new(),
        };

        match self.peek() {
            Some(b',') => {
                self.offset += 1;
                result.pane_index = Some(self.number_u32()?);
            }
            Some(open @ (b'{' | b'[')) => {
                self.offset += 1;
                let close = if open == b'{' { b'}' } else { b']' };
                result.axis = Some(if open == b'{' {
                    LayoutAxis::Horizontal
                } else {
                    LayoutAxis::Vertical
                });
                loop {
                    result.children.push(self.node()?);
                    match self.peek() {
                        Some(b',') => self.offset += 1,
                        Some(byte) if byte == close => {
                            self.offset += 1;
                            break;
                        }
                        _ => return Err(LayoutParseError::Invalid(self.offset)),
                    }
                }
            }
            _ => return Err(LayoutParseError::Invalid(self.offset)),
        }

        Ok(result)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.offset).copied()
    }

    fn expect(&mut self, expected: u8) -> Result<(), LayoutParseError> {
        if self.peek() != Some(expected) {
            return Err(LayoutParseError::Invalid(self.offset));
        }
        self.offset += 1;
        Ok(())
    }

    fn number_u16(&mut self) -> Result<u16, LayoutParseError> {
        self.number_u32()?
            .try_into()
            .map_err(|_| LayoutParseError::Invalid(self.offset))
    }

    fn number_u32(&mut self) -> Result<u32, LayoutParseError> {
        let start = self.offset;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.offset += 1;
        }
        if start == self.offset {
            return Err(LayoutParseError::Invalid(self.offset));
        }
        std::str::from_utf8(&self.bytes[start..self.offset])
            .ok()
            .and_then(|value| value.parse().ok())
            .ok_or(LayoutParseError::Invalid(start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recursive_split_layout() {
        let layout = "b25d,160x48,0,0{79x48,0,0,0,80x48,80,0[80x23,80,0,1,80x24,80,24,2]}";
        let result = parse_layout(layout).unwrap();
        assert_eq!(result.axis, Some(LayoutAxis::Horizontal));
        assert_eq!(result.children[0].pane_index, Some(0));
        assert_eq!(result.children[1].axis, Some(LayoutAxis::Vertical));
        assert_eq!(result.children[1].children[1].pane_index, Some(2));
    }

    #[test]
    fn rejects_trailing_or_incomplete_layouts() {
        assert!(matches!(
            parse_layout("ffff,80x24,0,0,0junk"),
            Err(LayoutParseError::Trailing(_))
        ));
        assert!(matches!(
            parse_layout("ffff,80x24,0,0{"),
            Err(LayoutParseError::Invalid(_))
        ));
    }

    /// tmux 3.8's `#{window_layout}`, with a floating pane (`z`) in the tree.
    #[test]
    fn parses_tmux_38_json_layout_as_the_checksummed_form() {
        let json = r#"{"V":2,"L":{"t":"h","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":40,"h":24,"x":0,"y":0,"l":1,"i":0,"I":"%0"},{"t":"v","w":39,"h":24,"x":41,"y":0,"c":[{"t":"p","w":39,"h":12,"x":41,"y":0,"a":true,"i":1,"I":"%1"},{"t":"p","w":39,"h":11,"x":41,"y":13,"l":0,"i":2,"I":"%7"}]}]}}"#;
        let classic = "0000,80x24,0,0{40x24,0,0,0,39x24,41,0[39x12,41,0,1,39x11,41,13,7]}";
        assert_eq!(parse_layout(json).unwrap(), parse_layout(classic).unwrap());

        let floating = r#"{"V":2,"L":{"t":"v","w":39,"h":24,"x":41,"y":0,"c":[{"t":"p","w":39,"h":24,"x":41,"y":0,"a":true,"i":1,"I":"%1"},{"t":"p","w":40,"h":6,"x":4,"y":2,"l":0,"i":2,"z":0,"I":"%2"}]}}"#;
        assert_eq!(
            parse_layout(floating).unwrap().children[1].pane_index,
            Some(2)
        );
    }

    #[test]
    fn rejects_malformed_or_unknown_json_layouts() {
        for layout in [
            r#"{"V":3,"L":{"t":"p","w":80,"h":24,"x":0,"y":0,"I":"%0"}}"#,
            r#"{"V":2,"L":{"t":"p","w":80,"h":24,"x":0,"y":0}}"#,
            r#"{"V":2,"L":{"t":"p","w":80,"h":24,"x":0,"y":0,"I":"0"}}"#,
            r#"{"V":2,"L":{"t":"h","w":80,"h":24,"x":0,"y":0,"c":[]}}"#,
            r#"{"V":2,"L":{"t":"q","w":80,"h":24,"x":0,"y":0,"I":"%0"}}"#,
            r#"{"V":2,"L":{"t":"p","w":80,"h":24,"x":0,"y":0,"I":"%0"}}junk"#,
        ] {
            assert!(
                matches!(parse_layout(layout), Err(LayoutParseError::Json(_))),
                "{layout}"
            );
        }
    }
}
