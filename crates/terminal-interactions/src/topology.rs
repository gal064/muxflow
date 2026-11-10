use serde_json::Value;
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneGrid {
    pub pane_id: String,
    pub columns: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TopologyNotification {
    pub name: String,
    /// Present only for `layout-change`.
    pub grids: Option<Vec<PaneGrid>>,
}

#[derive(Debug, thiserror::Error)]
pub enum TopologyError {
    #[error("layout notification omitted pane grids")]
    MissingGrids,
    #[error("layout notification pane grids are not valid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("layout notification pane grids are not an array")]
    NotAnArray,
    #[error("invalid pane grid")]
    InvalidGrid,
}

/// Layout sizes travel in the existing ordered topology-notification frame.
pub fn decode_topology_notification(label: &str) -> Result<TopologyNotification, TopologyError> {
    if !label.starts_with("layout-change") {
        return Ok(TopologyNotification {
            name: label.to_string(),
            grids: None,
        });
    }
    let Some(payload) = label.strip_prefix("layout-change ") else {
        return Err(TopologyError::MissingGrids);
    };
    let Value::Array(entries) = serde_json::from_str(payload)? else {
        return Err(TopologyError::NotAnArray);
    };
    let mut seen = HashSet::new();
    let mut grids = Vec::with_capacity(entries.len());
    for entry in &entries {
        let grid = decode_grid(entry).ok_or(TopologyError::InvalidGrid)?;
        if !seen.insert(grid.pane_id.clone()) {
            return Err(TopologyError::InvalidGrid);
        }
        grids.push(grid);
    }
    Ok(TopologyNotification {
        name: "layout-change".to_string(),
        grids: Some(grids),
    })
}

fn decode_grid(entry: &Value) -> Option<PaneGrid> {
    let [pane_id, columns, rows] = entry.as_array()?.as_slice() else {
        return None;
    };
    let pane_id = pane_id.as_str()?;
    let digits = pane_id.strip_prefix('%')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(PaneGrid {
        pane_id: pane_id.to_string(),
        columns: grid_dimension(columns)?,
        rows: grid_dimension(rows)?,
    })
}

/// JavaScript reads `80.0` and `8e1` as the integer 80, so a float with no
/// fractional part is accepted too.
fn grid_dimension(value: &Value) -> Option<u16> {
    let number = value.as_f64()?;
    ((1.0..=65535.0).contains(&number) && number.fract() == 0.0).then_some(number as u16)
}
