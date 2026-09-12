use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::exec::AppResult;
use crate::mediainfo::HybridMediaInfo;

use super::align::AlignmentStrategy;
use super::letterbox::{ActiveAreaChoice, Bars};
use super::mapping::MappingPolicy;

/// dovi_tool editor config (`editor -j`). Field names and shapes mirror
/// `EditConfig` in dovi_tool/src/dovi/editor.rs, which deserializes with
/// `deny_unknown_fields` — any drift fails loudly at the editor step.
#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct EditorConfig {
    pub(crate) mode: u8,
    pub(crate) remove_cmv4: bool,
    pub(crate) remove_mapping: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) remove: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) duplicate: Option<Vec<DuplicateEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) active_area: Option<ActiveArea>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) level6: Option<Level6Meta>,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub(crate) struct DuplicateEntry {
    pub(crate) source: u64,
    pub(crate) offset: u64,
    pub(crate) length: u64,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub(crate) struct ActiveArea {
    pub(crate) crop: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) presets: Option<Vec<ActiveAreaPreset>>,
    /// BTreeMap keeps serialized key order deterministic for snapshots.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) edits: Option<BTreeMap<String, u16>>,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub(crate) struct ActiveAreaPreset {
    pub(crate) id: u16,
    pub(crate) left: u16,
    pub(crate) right: u16,
    pub(crate) top: u16,
    pub(crate) bottom: u16,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Level6Meta {
    pub(crate) max_display_mastering_luminance: u16,
    pub(crate) min_display_mastering_luminance: u16,
    pub(crate) max_content_light_level: u16,
    pub(crate) max_frame_average_light_level: u16,
}

pub(crate) fn l6_from_media_info(info: &HybridMediaInfo) -> Option<Level6Meta> {
    let max_cll = info.max_cll?;
    let max_fall = info.max_fall?;
    let min_nits = info.mastering_min_nits?;
    let max_nits = info.mastering_max_nits?;

    let mut min_display = if min_nits <= 1.0 {
        (min_nits * 10000.0).round() as u16
    } else {
        min_nits.round() as u16
    };
    if min_display == 0 {
        min_display = 1;
    }

    let max_display = max_nits.round().clamp(1.0, 10000.0) as u16;

    Some(Level6Meta {
        max_display_mastering_luminance: max_display,
        min_display_mastering_luminance: min_display.min(10000),
        max_content_light_level: max_cll.min(10000),
        max_frame_average_light_level: max_fall.min(10000),
    })
}

fn preset_from_bars(id: u16, b: &Bars) -> ActiveAreaPreset {
    ActiveAreaPreset {
        id,
        left: b.left.min(u16::MAX as u32) as u16,
        right: b.right.min(u16::MAX as u32) as u16,
        top: b.top.min(u16::MAX as u32) as u16,
        bottom: b.bottom.min(u16::MAX as u32) as u16,
    }
}

fn edits_all(id: u16) -> BTreeMap<String, u16> {
    let mut edits = BTreeMap::new();
    edits.insert("all".to_string(), id);
    edits
}

/// Legacy fallback: derive the active area from the resolution difference
/// between the two sources when no measurement is available.
pub(crate) fn resolution_active_area(
    dv_info: &HybridMediaInfo,
    hdr_info: &HybridMediaInfo,
) -> Option<ActiveArea> {
    let (Some(dw), Some(dh), Some(hw), Some(hh)) = (
        dv_info.width,
        dv_info.height,
        hdr_info.width,
        hdr_info.height,
    ) else {
        return None;
    };

    if dw == hw && dh == hh {
        return None;
    }

    let target_bigger_or_equal = hw >= dw && hh >= dh;

    if target_bigger_or_equal {
        // Target canvas is larger: the picture sits centered inside it, so
        // the L5 offsets are half the canvas delta on each axis.
        let bars = Bars {
            left: (hw - dw) / 2,
            right: (hw - dw) / 2,
            top: (hh - dh) / 2,
            bottom: (hh - dh) / 2,
        };
        return Some(ActiveArea {
            crop: false,
            presets: Some(vec![preset_from_bars(1, &bars)]),
            edits: Some(edits_all(1)),
        });
    }

    // Target smaller or mixed: zero out L5 so stale source offsets don't
    // misplace the active area on the new canvas.
    Some(ActiveArea {
        crop: true,
        presets: None,
        edits: None,
    })
}

/// Assemble the full editor config. Pure — snapshot-tested below.
pub(crate) fn build_editor_config(
    strategy: &AlignmentStrategy,
    mapping_policy: MappingPolicy,
    dv_info: &HybridMediaInfo,
    hdr_info: &HybridMediaInfo,
    active_area: &ActiveAreaChoice,
) -> EditorConfig {
    let mode = mapping_policy.editor_mode();

    let remove = if strategy.remove_ranges.is_empty() {
        None
    } else {
        Some(strategy.remove_ranges.clone())
    };

    let duplicate = if strategy.duplicates.is_empty() {
        None
    } else {
        Some(
            strategy
                .duplicates
                .iter()
                .map(|d| DuplicateEntry {
                    source: d.source,
                    offset: d.offset,
                    length: d.length,
                })
                .collect(),
        )
    };

    let active_area = match active_area {
        ActiveAreaChoice::Keep => None,
        ActiveAreaChoice::Resolution => resolution_active_area(dv_info, hdr_info),
        ActiveAreaChoice::Measured(b) => Some(ActiveArea {
            crop: true,
            presets: Some(vec![preset_from_bars(1, b)]),
            edits: Some(edits_all(1)),
        }),
    };

    let dv_l6 = l6_from_media_info(dv_info);
    let hdr_l6 = l6_from_media_info(hdr_info);
    let level6 = hdr_l6.filter(|target| dv_l6.as_ref() != Some(target));

    EditorConfig {
        mode,
        remove_cmv4: false,
        remove_mapping: false,
        remove,
        duplicate,
        active_area,
        level6,
    }
}

pub(crate) fn hybrid_build_editor_json(
    strategy: &AlignmentStrategy,
    mapping_policy: MappingPolicy,
    dv_info: &HybridMediaInfo,
    hdr_info: &HybridMediaInfo,
    active_area: &ActiveAreaChoice,
    json_output_path: &Path,
) -> AppResult<()> {
    let config = build_editor_config(strategy, mapping_policy, dv_info, hdr_info, active_area);

    let mut json = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize editor config: {e}"))?;
    json.push('\n');

    fs::write(json_output_path, json).map_err(|e| {
        format!(
            "Failed to write editor JSON {}: {e}",
            json_output_path.display()
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hybrid::align::DuplicateOp;

    fn info(w: u32, h: u32) -> HybridMediaInfo {
        HybridMediaInfo {
            width: Some(w),
            height: Some(h),
            ..Default::default()
        }
    }

    fn info_with_l6(w: u32, h: u32, max_nits: f64, max_cll: u16) -> HybridMediaInfo {
        HybridMediaInfo {
            width: Some(w),
            height: Some(h),
            max_cll: Some(max_cll),
            max_fall: Some(400),
            mastering_min_nits: Some(0.005),
            mastering_max_nits: Some(max_nits),
            ..Default::default()
        }
    }

    fn strategy(remove: &[&str], dups: &[(u64, u64, u64)]) -> AlignmentStrategy {
        AlignmentStrategy {
            remove_ranges: remove.iter().map(|s| s.to_string()).collect(),
            duplicates: dups
                .iter()
                .map(|&(source, offset, length)| DuplicateOp {
                    source,
                    offset,
                    length,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn render(config: &EditorConfig) -> String {
        serde_json::to_string_pretty(config).unwrap()
    }

    #[test]
    fn snapshot_mode2_minimal() {
        let c = build_editor_config(
            &strategy(&[], &[]),
            MappingPolicy::Profile7Compatibility,
            &info(3840, 2160),
            &info(3840, 2160),
            &ActiveAreaChoice::Keep,
        );
        assert_eq!(
            render(&c),
            r#"{
  "mode": 2,
  "remove_cmv4": false,
  "remove_mapping": false
}"#
        );
    }

    #[test]
    fn sync_repair_preserves_existing_mapping() {
        let c = build_editor_config(
            &strategy(&[], &[]),
            MappingPolicy::PreserveForSyncRepair,
            &info(3840, 2160),
            &info(3840, 2160),
            &ActiveAreaChoice::Keep,
        );
        assert_eq!(c.mode, 0);
        assert!(!c.remove_mapping);
        assert!(render(&c).contains("\"mode\": 0"));
    }

    #[test]
    fn snapshot_positive_offset_remove_ranges() {
        let c = build_editor_config(
            &strategy(&["0-24", "1127-1151"], &[]),
            MappingPolicy::Profile8Identity,
            &info(3840, 2160),
            &info(3840, 2160),
            &ActiveAreaChoice::Keep,
        );
        assert_eq!(
            render(&c),
            r#"{
  "mode": 0,
  "remove_cmv4": false,
  "remove_mapping": false,
  "remove": [
    "0-24",
    "1127-1151"
  ]
}"#
        );
    }

    #[test]
    fn snapshot_negative_offset_duplicates() {
        let c = build_editor_config(
            &strategy(&[], &[(0, 0, 25)]),
            MappingPolicy::Profile8Identity,
            &info(3840, 2160),
            &info(3840, 2160),
            &ActiveAreaChoice::Keep,
        );
        assert_eq!(
            render(&c),
            r#"{
  "mode": 0,
  "remove_cmv4": false,
  "remove_mapping": false,
  "duplicate": [
    {
      "source": 0,
      "offset": 0,
      "length": 25
    }
  ]
}"#
        );
    }

    #[test]
    fn snapshot_mixed_remove_and_duplicate() {
        // Negative start offset (pad) plus end pad: two duplicates; or
        // start trim plus end pad: remove + duplicate. Cover the latter.
        let c = build_editor_config(
            &strategy(&["0-9"], &[(1141, 1142, 5)]),
            MappingPolicy::Profile7Compatibility,
            &info(3840, 2160),
            &info(3840, 2160),
            &ActiveAreaChoice::Keep,
        );
        assert_eq!(
            render(&c),
            r#"{
  "mode": 2,
  "remove_cmv4": false,
  "remove_mapping": false,
  "remove": [
    "0-9"
  ],
  "duplicate": [
    {
      "source": 1141,
      "offset": 1142,
      "length": 5
    }
  ]
}"#
        );
    }

    #[test]
    fn snapshot_measured_letterbox() {
        let c = build_editor_config(
            &strategy(&[], &[]),
            MappingPolicy::Profile8Identity,
            &info(3840, 2160),
            &info(3840, 2160),
            &ActiveAreaChoice::Measured(Bars {
                left: 0,
                right: 0,
                top: 276,
                bottom: 276,
            }),
        );
        assert_eq!(
            render(&c),
            r#"{
  "mode": 0,
  "remove_cmv4": false,
  "remove_mapping": false,
  "active_area": {
    "crop": true,
    "presets": [
      {
        "id": 1,
        "left": 0,
        "right": 0,
        "top": 276,
        "bottom": 276
      }
    ],
    "edits": {
      "all": 1
    }
  }
}"#
        );
    }

    #[test]
    fn snapshot_resolution_target_bigger() {
        let c = build_editor_config(
            &strategy(&[], &[]),
            MappingPolicy::Profile7Compatibility,
            &info(3840, 1608),
            &info(3840, 2160),
            &ActiveAreaChoice::Resolution,
        );
        assert_eq!(
            render(&c),
            r#"{
  "mode": 2,
  "remove_cmv4": false,
  "remove_mapping": false,
  "active_area": {
    "crop": false,
    "presets": [
      {
        "id": 1,
        "left": 0,
        "right": 0,
        "top": 276,
        "bottom": 276
      }
    ],
    "edits": {
      "all": 1
    }
  }
}"#
        );
    }

    #[test]
    fn resolution_target_smaller_crops_only() {
        let aa = resolution_active_area(&info(3840, 2160), &info(1920, 1080)).unwrap();
        assert!(aa.crop);
        assert!(aa.presets.is_none());
        assert!(aa.edits.is_none());
    }

    #[test]
    fn resolution_same_canvas_none() {
        assert!(resolution_active_area(&info(3840, 2160), &info(3840, 2160)).is_none());
        let missing = HybridMediaInfo::default();
        assert!(resolution_active_area(&missing, &info(3840, 2160)).is_none());
    }

    #[test]
    fn snapshot_level6_override_when_differs() {
        let c = build_editor_config(
            &strategy(&[], &[]),
            MappingPolicy::Profile8Identity,
            &info_with_l6(3840, 2160, 4000.0, 4000),
            &info_with_l6(3840, 2160, 1000.0, 1000),
            &ActiveAreaChoice::Keep,
        );
        assert_eq!(
            render(&c),
            r#"{
  "mode": 0,
  "remove_cmv4": false,
  "remove_mapping": false,
  "level6": {
    "max_display_mastering_luminance": 1000,
    "min_display_mastering_luminance": 50,
    "max_content_light_level": 1000,
    "max_frame_average_light_level": 400
  }
}"#
        );
    }

    #[test]
    fn level6_skipped_when_equal() {
        let c = build_editor_config(
            &strategy(&[], &[]),
            MappingPolicy::Profile8Identity,
            &info_with_l6(3840, 2160, 1000.0, 1000),
            &info_with_l6(3840, 2160, 1000.0, 1000),
            &ActiveAreaChoice::Keep,
        );
        assert!(c.level6.is_none());
    }

    #[test]
    fn level6_applied_when_dv_missing() {
        let c = build_editor_config(
            &strategy(&[], &[]),
            MappingPolicy::Profile8Identity,
            &info(3840, 2160),
            &info_with_l6(3840, 2160, 1000.0, 1000),
            &ActiveAreaChoice::Keep,
        );
        assert_eq!(
            c.level6,
            Some(Level6Meta {
                max_display_mastering_luminance: 1000,
                min_display_mastering_luminance: 50,
                max_content_light_level: 1000,
                max_frame_average_light_level: 400,
            })
        );
    }

    #[test]
    fn l6_min_luminance_scaling() {
        // Fractional nits are stored as 1/10000 nit units; integer nits kept.
        let frac = info_with_l6(1, 1, 1000.0, 1000);
        assert_eq!(
            l6_from_media_info(&frac)
                .unwrap()
                .min_display_mastering_luminance,
            50
        );
        let mut int_nits = info_with_l6(1, 1, 1000.0, 1000);
        int_nits.mastering_min_nits = Some(5.0);
        assert_eq!(
            l6_from_media_info(&int_nits)
                .unwrap()
                .min_display_mastering_luminance,
            5
        );
        let mut zero = info_with_l6(1, 1, 1000.0, 1000);
        zero.mastering_min_nits = Some(0.00001);
        assert_eq!(
            l6_from_media_info(&zero)
                .unwrap()
                .min_display_mastering_luminance,
            1
        );
    }
}
