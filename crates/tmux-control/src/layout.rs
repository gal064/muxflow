use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Coalesces layout reconciliation without losing a mutation that arrives
/// while a pass is in flight. Durable facts are always read at the start of a
/// pass and exactly one follow-up is requested when its generation changes.
#[derive(Debug, Default, Clone)]
pub struct LayoutGeneration {
    dirty_generation: u64,
    reconciling: bool,
}

impl LayoutGeneration {
    pub fn mark_dirty(&mut self) -> u64 {
        self.dirty_generation = self.dirty_generation.saturating_add(1);
        self.dirty_generation
    }

    pub fn begin(&mut self) -> Option<u64> {
        if self.reconciling {
            return None;
        }
        self.reconciling = true;
        Some(self.dirty_generation)
    }

    /// Returns true when one follow-up pass is required.
    pub fn finish(&mut self, started_at: u64) -> bool {
        self.reconciling = false;
        self.dirty_generation != started_at
    }

    pub fn current(&self) -> u64 {
        self.dirty_generation
    }
}

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
}

pub fn parse_layout(layout: &str) -> Result<LayoutNode, LayoutParseError> {
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

    #[test]
    fn generation_schedules_one_follow_up_for_changes_during_a_pass() {
        let mut generation = LayoutGeneration::default();
        generation.mark_dirty();
        let started = generation.begin().unwrap();
        assert!(generation.begin().is_none());
        generation.mark_dirty();
        generation.mark_dirty();
        assert!(generation.finish(started));
        let follow_up = generation.begin().unwrap();
        assert!(!generation.finish(follow_up));
    }
}
