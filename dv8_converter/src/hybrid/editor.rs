use std::fs;
use std::path::Path;

use crate::exec::AppResult;
use crate::mediainfo::HybridMediaInfo;

use super::align::AlignmentStrategy;
use super::letterbox::ActiveAreaChoice;

#[derive(Clone, PartialEq, Eq)]
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

pub(crate) fn build_active_area_json(
    dv_info: &HybridMediaInfo,
    hdr_info: &HybridMediaInfo,
) -> Option<String> {
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
    let target_smaller_or_equal = hw <= dw && hh <= dh;

    if target_bigger_or_equal && (hw > dw || hh > dh) {
        let left = (hw - dw) / 2;
        let right = (hw - dw) / 2;
        let top = (hh - dh) / 2;
        let bottom = (hh - dh) / 2;

        return Some(format!(
            "{{\n    \"presets\": [{{\"id\": 1, \"left\": {left}, \"right\": {right}, \"top\": {top}, \"bottom\": {bottom}}}],\n    \"edits\": {{\"all\": 1}}\n  }}"
        ));
    }

    if target_smaller_or_equal && (hw < dw || hh < dh) {
        return Some("{\"crop\": true}".to_string());
    }

    Some("{\"crop\": true}".to_string())
}

pub(crate) fn hybrid_build_editor_json(
    strategy: &AlignmentStrategy,
    dv_profile: Option<u8>,
    dv_info: &HybridMediaInfo,
    hdr_info: &HybridMediaInfo,
    active_area: &ActiveAreaChoice,
    json_output_path: &Path,
) -> AppResult<()> {
    let mode = if dv_profile == Some(5) { 3 } else { 2 };

    let mut fields: Vec<String> = Vec::new();
    fields.push(format!("  \"mode\": {mode}"));
    fields.push("  \"remove_cmv4\": false".to_string());
    fields.push("  \"remove_mapping\": true".to_string());

    if !strategy.remove_ranges.is_empty() {
        let values = strategy
            .remove_ranges
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ");
        fields.push(format!("  \"remove\": [{values}]"));
    }

    if !strategy.duplicates.is_empty() {
        let mut dup_json = String::from("  \"duplicate\": [\n");
        for (idx, d) in strategy.duplicates.iter().enumerate() {
            let comma = if idx + 1 == strategy.duplicates.len() {
                ""
            } else {
                ","
            };
            dup_json.push_str(&format!(
                "    {{\"source\": {}, \"offset\": {}, \"length\": {}}}{}\n",
                d.source, d.offset, d.length, comma
            ));
        }
        dup_json.push_str("  ]");
        fields.push(dup_json);
    }

    match active_area {
        ActiveAreaChoice::Keep => {}
        ActiveAreaChoice::Resolution => {
            if let Some(active_area_json) = build_active_area_json(dv_info, hdr_info) {
                fields.push(format!("  \"active_area\": {active_area_json}"));
            }
        }
        ActiveAreaChoice::Measured(b) => {
            fields.push(format!(
                "  \"active_area\": {{\n    \"crop\": true,\n    \"presets\": [{{\"id\": 1, \"left\": {}, \"right\": {}, \"top\": {}, \"bottom\": {}}}],\n    \"edits\": {{\"all\": 1}}\n  }}",
                b.left, b.right, b.top, b.bottom
            ));
        }
    }

    let dv_l6 = l6_from_media_info(dv_info);
    let hdr_l6 = l6_from_media_info(hdr_info);
    if let Some(target_l6) = hdr_l6 {
        let should_override = dv_l6.as_ref().map(|src| src != &target_l6).unwrap_or(true);

        if should_override {
            fields.push(format!(
                "  \"level6\": {{\"max_display_mastering_luminance\": {}, \"min_display_mastering_luminance\": {}, \"max_content_light_level\": {}, \"max_frame_average_light_level\": {}}}",
                target_l6.max_display_mastering_luminance,
                target_l6.min_display_mastering_luminance,
                target_l6.max_content_light_level,
                target_l6.max_frame_average_light_level
            ));
        }
    }

    let json = format!("{{\n{}\n}}\n", fields.join(",\n"));
    fs::write(json_output_path, json).map_err(|e| {
        format!(
            "Failed to write editor JSON {}: {e}",
            json_output_path.display()
        )
    })?;

    Ok(())
}
