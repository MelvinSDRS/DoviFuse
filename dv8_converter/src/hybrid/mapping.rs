//! Whole-RPU mapping eligibility checks for hybrid conversion.
//!
//! The pinned `dovi_tool` exporter writes a JSON array of parsed RPUs.  The
//! array is consumed one item at a time.  The supported transfer contract is
//! intentionally narrow: Profile 8 and Profile 7 MEL require the known no-op
//! mapping; Profile 7 FEL is accepted only as an explicit mode-2 conversion,
//! which discards the FEL picture residuals and mapping.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use serde::de::{self, IgnoredAny, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Deserializer as JsonDeserializer;

use crate::exec::{run_status, AppResult};
use crate::logger::Logger;
use crate::runtime::Runtime;

const COMPONENTS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MappingPolicy {
    Profile8Identity,
    Profile7Compatibility,
    PreserveForSyncRepair,
}

impl MappingPolicy {
    pub(crate) fn editor_mode(self) -> u8 {
        match self {
            Self::Profile8Identity | Self::PreserveForSyncRepair => 0,
            Self::Profile7Compatibility => 2,
        }
    }
}

/// Export and inspect every RPU. `export_path` belongs to the caller's
/// cleanup guard; this function does not create or remove it.
pub(crate) fn inspect(
    rpu: &Path,
    export_path: &Path,
    profile: Option<u8>,
    frames: u64,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<MappingPolicy> {
    let profile =
        profile.ok_or_else(|| "Mapping inspection requires a known DV profile".to_string())?;
    if !matches!(profile, 7 | 8) {
        return Err(format!(
            "Mapping inspection does not support DV profile {profile}"
        ));
    }
    if frames == 0 {
        return Err("Mapping inspection requires a nonzero RPU frame count".to_string());
    }

    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("export"),
            OsString::from("-i"),
            rpu.as_os_str().to_os_string(),
            OsString::from("-d"),
            OsString::from(format!("all={}", export_path.display())),
        ],
    )?;

    let file = File::open(export_path).map_err(|e| {
        format!(
            "Failed to read dovi_tool mapping export {}: {e}",
            export_path.display()
        )
    })?;
    let result = inspect_reader(BufReader::new(file), profile, frames).map_err(|e| {
        format!(
            "Invalid dovi_tool mapping export {}: {e}",
            export_path.display()
        )
    })?;

    let policy_name = match result.policy {
        MappingPolicy::Profile8Identity => "Profile8Identity",
        MappingPolicy::Profile7Compatibility => "Profile7Compatibility",
        MappingPolicy::PreserveForSyncRepair => "PreserveForSyncRepair",
    };
    logger.measurement(
        "mapping_policy",
        serde_json::json!({
            "policy": policy_name,
            "coverage": "all exported RPUs",
            "rpu_count": result.stats.count,
            "expected_frames": frames,
            "identity_p8_rpus": result.stats.identity_p8,
            "identity_mel_rpus": result.stats.identity_mel,
            "fel_compatibility_rpus": result.stats.fel,
            "fel_mapping_discarded_by_mode2": result.stats.fel > 0,
            "retained_sources": "caller-controlled; this inspection does not delete sources",
            "creative_validation": "not performed",
        }),
    );
    if result.stats.fel > 0 {
        logger.warn(&format!(
            "Mapping policy: Profile 7 FEL compatibility accepted for all {} RPUs; mode 2 discards FEL residuals and mapping. This is metadata eligibility only and makes no creative/color validation claim. Source retention remains caller-controlled.",
            result.stats.fel
        ));
    } else {
        logger.ok(&format!(
            "Mapping policy: {policy_name}; inspected all {} RPUs. This is metadata eligibility only and makes no creative/color validation claim. Source retention remains caller-controlled.",
            result.stats.count
        ));
    }
    Ok(result.policy)
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
enum ElType {
    #[serde(rename = "MEL")]
    Mel,
    #[serde(rename = "FEL")]
    Fel,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
enum MappingMethod {
    Polynomial,
    #[serde(rename = "MMR")]
    Mmr,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
enum NlqMethod {
    LinearDeadzone,
}

#[derive(Debug, Deserialize)]
struct ExportedRpu {
    dovi_profile: u8,
    #[serde(default)]
    el_type: Option<ElType>,
    header: Header,
    rpu_data_mapping: Option<Mapping>,
}

#[derive(Debug, Deserialize)]
struct Header {
    rpu_nal_prefix: u8,
    rpu_type: u8,
    rpu_format: u16,
    vdr_rpu_profile: u8,
    vdr_rpu_level: u8,
    vdr_seq_info_present_flag: bool,
    chroma_resampling_explicit_filter_flag: bool,
    coefficient_data_type: u8,
    coefficient_log2_denom: u64,
    coefficient_log2_denom_length: u32,
    vdr_rpu_normalized_idc: u8,
    bl_video_full_range_flag: bool,
    bl_bit_depth_minus8: u64,
    el_bit_depth_minus8: u64,
    ext_mapping_idc_0_4: u8,
    ext_mapping_idc_5_7: u8,
    vdr_bit_depth_minus8: u64,
    spatial_resampling_filter_flag: bool,
    reserved_zero_3bits: u8,
    el_spatial_resampling_filter_flag: bool,
    disable_residual_flag: bool,
    vdr_dm_metadata_present_flag: bool,
    use_prev_vdr_rpu_flag: bool,
    prev_vdr_rpu_id: u64,
    #[serde(flatten)]
    extra: BTreeMap<String, IgnoredAny>,
}

#[derive(Debug, Deserialize)]
struct Mapping {
    vdr_rpu_id: u64,
    mapping_color_space: u64,
    mapping_chroma_format_idc: u64,
    num_x_partitions_minus1: u64,
    num_y_partitions_minus1: u64,
    curves: Vec<Curve>,
    #[serde(default)]
    nlq_method_idc: Option<NlqMethod>,
    #[serde(default)]
    nlq_num_pivots_minus2: Option<u8>,
    #[serde(default)]
    nlq_pred_pivot_value: Option<Vec<u16>>,
    #[serde(default)]
    nlq: Option<Nlq>,
    #[serde(flatten)]
    extra: BTreeMap<String, IgnoredAny>,
}

#[derive(Debug, Deserialize)]
struct Curve {
    num_pivots_minus2: u64,
    pivots: Vec<u16>,
    mapping_idc: MappingMethod,
    #[serde(default)]
    poly_order_minus1: Option<Vec<u64>>,
    #[serde(default)]
    linear_interp_flag: Option<Vec<bool>>,
    #[serde(default)]
    poly_coef_int: Option<Vec<Vec<i64>>>,
    #[serde(default)]
    poly_coef: Option<Vec<Vec<u64>>>,
    #[serde(flatten)]
    extra: BTreeMap<String, IgnoredAny>,
}

#[derive(Debug, Deserialize)]
struct Nlq {
    nlq_offset: Vec<u16>,
    vdr_in_max_int: Vec<u64>,
    vdr_in_max: Vec<u64>,
    linear_deadzone_slope_int: Vec<u64>,
    linear_deadzone_slope: Vec<u64>,
    linear_deadzone_threshold_int: Vec<u64>,
    linear_deadzone_threshold: Vec<u64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Stats {
    count: u64,
    identity_p8: u64,
    identity_mel: u64,
    fel: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Inspection {
    policy: MappingPolicy,
    stats: Stats,
}

struct MappingVisitor {
    profile: u8,
    frames: u64,
}

impl<'de> Visitor<'de> for MappingVisitor {
    type Value = Stats;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON array of exported Dolby Vision RPU objects")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Stats, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut stats = Stats::default();
        while let Some(rpu) = seq.next_element::<ExportedRpu>()? {
            if stats.count >= self.frames {
                return Err(de::Error::custom(format!(
                    "RPU export contains more than the expected {} frames",
                    self.frames
                )));
            }
            inspect_one(&rpu, self.profile, &mut stats).map_err(de::Error::custom)?;
            stats.count += 1;
        }
        if stats.count != self.frames {
            return Err(de::Error::custom(format!(
                "RPU export contains {} frames; expected {}",
                stats.count, self.frames
            )));
        }
        Ok(stats)
    }
}

fn inspect_reader<R: Read>(reader: R, profile: u8, frames: u64) -> AppResult<Inspection> {
    if frames == 0 {
        return Err("Mapping inspection requires a nonzero RPU frame count".to_string());
    }
    let mut de = JsonDeserializer::from_reader(reader);
    let stats = de
        .deserialize_seq(MappingVisitor { profile, frames })
        .map_err(|e| e.to_string())?;
    de.end().map_err(|e| e.to_string())?;

    let policy = match profile {
        8 if stats.identity_p8 == frames => MappingPolicy::Profile8Identity,
        7 if stats.identity_mel + stats.fel == frames => MappingPolicy::Profile7Compatibility,
        7 | 8 => return Err("mapping policy coverage is incomplete".to_string()),
        other => {
            return Err(format!(
                "Mapping inspection does not support DV profile {other}"
            ))
        }
    };
    Ok(Inspection { policy, stats })
}

fn inspect_one(rpu: &ExportedRpu, profile: u8, stats: &mut Stats) -> AppResult<()> {
    if rpu.dovi_profile != profile {
        return Err(format!(
            "mixed or unexpected RPU profiles: found {}, expected {} at frame {}",
            rpu.dovi_profile, profile, stats.count
        ));
    }
    if rpu.header.use_prev_vdr_rpu_flag || rpu.header.prev_vdr_rpu_id != 0 {
        return Err(format!(
            "RPU frame {} reuses a previous mapping; reusable mappings are unsupported",
            stats.count
        ));
    }
    validate_header(&rpu.header, profile)?;
    let mapping = rpu
        .rpu_data_mapping
        .as_ref()
        .ok_or_else(|| format!("RPU frame {} has no rpu_data_mapping", stats.count))?;
    validate_mapping_fields(mapping)?;

    match profile {
        8 => {
            if rpu.el_type.is_some() {
                return Err(format!("Profile 8 RPU frame {} has el_type", stats.count));
            }
            validate_identity(mapping)
                .map_err(|e| format!("RPU frame {} mapping: {e}", stats.count))?;
            if mapping.nlq_method_idc.is_some()
                || mapping.nlq_num_pivots_minus2.is_some()
                || mapping.nlq_pred_pivot_value.is_some()
                || mapping.nlq.is_some()
            {
                return Err(format!(
                    "Profile 8 RPU frame {} has NLQ metadata",
                    stats.count
                ));
            }
            stats.identity_p8 += 1;
        }
        7 => {
            let el_type = rpu
                .el_type
                .ok_or_else(|| format!("Profile 7 RPU frame {} is missing el_type", stats.count))?;
            validate_p7_nlq(mapping, stats.count)?;
            match el_type {
                ElType::Mel => {
                    validate_identity(mapping)
                        .map_err(|e| format!("RPU frame {} mapping: {e}", stats.count))?;
                    if !canonical_mel_nlq(mapping) {
                        return Err(format!(
                            "Profile 7 MEL RPU frame {} has noncanonical NLQ",
                            stats.count
                        ));
                    }
                    stats.identity_mel += 1;
                }
                ElType::Fel => {
                    if canonical_mel_nlq(mapping) {
                        return Err(format!(
                            "Profile 7 FEL RPU frame {} has canonical MEL NLQ",
                            stats.count
                        ));
                    }
                    stats.fel += 1;
                }
            }
        }
        _ => {
            return Err(format!(
                "Mapping inspection does not support DV profile {profile}"
            ))
        }
    }
    Ok(())
}

fn validate_header(h: &Header, profile: u8) -> AppResult<()> {
    if !h.extra.is_empty() {
        return Err(format!(
            "unsupported header fields: {}",
            h.extra.keys().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    let common = [
        ("rpu_nal_prefix", h.rpu_nal_prefix == 25),
        ("rpu_type", h.rpu_type == 2),
        ("rpu_format", h.rpu_format == 18),
        ("vdr_rpu_profile", h.vdr_rpu_profile == 1),
        ("vdr_rpu_level", h.vdr_rpu_level == 0),
        ("vdr_seq_info_present_flag", h.vdr_seq_info_present_flag),
        (
            "chroma_resampling_explicit_filter_flag",
            !h.chroma_resampling_explicit_filter_flag,
        ),
        ("coefficient_data_type", h.coefficient_data_type == 0),
        ("coefficient_log2_denom", h.coefficient_log2_denom == 23),
        (
            "coefficient_log2_denom_length",
            h.coefficient_log2_denom_length == 23,
        ),
        ("vdr_rpu_normalized_idc", h.vdr_rpu_normalized_idc == 1),
        ("bl_video_full_range_flag", !h.bl_video_full_range_flag),
        ("bl_bit_depth_minus8", h.bl_bit_depth_minus8 == 2),
        ("el_bit_depth_minus8", h.el_bit_depth_minus8 == 2),
        ("ext_mapping_idc_0_4", h.ext_mapping_idc_0_4 == 0),
        ("ext_mapping_idc_5_7", h.ext_mapping_idc_5_7 == 0),
        ("vdr_bit_depth_minus8", h.vdr_bit_depth_minus8 == 4),
        (
            "spatial_resampling_filter_flag",
            !h.spatial_resampling_filter_flag,
        ),
        ("reserved_zero_3bits", h.reserved_zero_3bits == 0),
        (
            "vdr_dm_metadata_present_flag",
            h.vdr_dm_metadata_present_flag,
        ),
    ];
    if let Some((name, false)) = common.into_iter().find(|(_, ok)| !ok) {
        return Err(format!("unsupported or noncanonical header field: {name}"));
    }
    let flags_ok = match profile {
        7 => h.el_spatial_resampling_filter_flag && !h.disable_residual_flag,
        8 => !h.el_spatial_resampling_filter_flag && h.disable_residual_flag,
        _ => false,
    };
    if !flags_ok {
        return Err(format!("header flags do not match Profile {profile}"));
    }
    Ok(())
}

fn validate_mapping_fields(mapping: &Mapping) -> AppResult<()> {
    if mapping.vdr_rpu_id > 15 {
        return Err("vdr_rpu_id is outside the Dolby Vision range".to_string());
    }
    if mapping.mapping_color_space != 0 || mapping.mapping_chroma_format_idc != 0 {
        return Err("unsupported mapping color-space or chroma-format flag".to_string());
    }
    if mapping.num_x_partitions_minus1 > 15 || mapping.num_y_partitions_minus1 > 15 {
        return Err("mapping partition count is outside the Dolby Vision range".to_string());
    }
    if mapping.curves.len() != COMPONENTS {
        return Err(format!(
            "mapping has {} curves; expected {COMPONENTS}",
            mapping.curves.len()
        ));
    }
    Ok(())
}

fn validate_identity(mapping: &Mapping) -> AppResult<()> {
    if !mapping.extra.is_empty() {
        return Err(format!(
            "unknown mapping fields: {}",
            mapping.extra.keys().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if mapping.num_x_partitions_minus1 != 0 || mapping.num_y_partitions_minus1 != 0 {
        return Err("identity mapping requires zero x/y partitions".to_string());
    }
    for (component, curve) in mapping.curves.iter().enumerate() {
        if !curve.extra.is_empty() {
            return Err(format!(
                "component {component} has unknown curve fields: {}",
                curve.extra.keys().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        let identity = curve.num_pivots_minus2 == 0
            && curve.pivots == [0, 1023]
            && curve.mapping_idc == MappingMethod::Polynomial
            && curve.poly_order_minus1.as_deref() == Some(&[0][..])
            && curve.linear_interp_flag.as_deref() == Some(&[false][..])
            && curve.poly_coef_int.as_deref() == Some(&[vec![0, 1]][..])
            && curve.poly_coef.as_deref() == Some(&[vec![0, 0]][..]);
        if !identity {
            return Err(format!(
                "component {component} is not the canonical identity mapping"
            ));
        }
    }
    Ok(())
}

fn validate_p7_nlq(mapping: &Mapping, frame: u64) -> AppResult<()> {
    if mapping.nlq_method_idc != Some(NlqMethod::LinearDeadzone)
        || mapping.nlq_num_pivots_minus2 != Some(0)
        || mapping.nlq_pred_pivot_value.as_deref() != Some(&[0, 1023][..])
    {
        return Err(format!(
            "Profile 7 RPU frame {frame} has unsupported NLQ fields"
        ));
    }
    let Some(nlq) = mapping.nlq.as_ref() else {
        return Err(format!("Profile 7 RPU frame {frame} has no NLQ data"));
    };
    let lengths = [
        nlq.nlq_offset.len(),
        nlq.vdr_in_max_int.len(),
        nlq.vdr_in_max.len(),
        nlq.linear_deadzone_slope_int.len(),
        nlq.linear_deadzone_slope.len(),
        nlq.linear_deadzone_threshold_int.len(),
        nlq.linear_deadzone_threshold.len(),
    ];
    if lengths.into_iter().any(|len| len != COMPONENTS) {
        return Err(format!(
            "Profile 7 RPU frame {frame} has incomplete NLQ channels"
        ));
    }
    Ok(())
}

fn canonical_mel_nlq(mapping: &Mapping) -> bool {
    let Some(nlq) = mapping.nlq.as_ref() else {
        return false;
    };
    mapping.nlq_method_idc == Some(NlqMethod::LinearDeadzone)
        && mapping.nlq_num_pivots_minus2 == Some(0)
        && mapping.nlq_pred_pivot_value.as_deref() == Some(&[0, 1023][..])
        && nlq.nlq_offset == [0, 0, 0]
        && nlq.vdr_in_max_int == [1, 1, 1]
        && nlq.vdr_in_max == [0, 0, 0]
        && nlq.linear_deadzone_slope_int == [0, 0, 0]
        && nlq.linear_deadzone_slope == [0, 0, 0]
        && nlq.linear_deadzone_threshold_int == [0, 0, 0]
        && nlq.linear_deadzone_threshold == [0, 0, 0]
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_json(name: &str) -> String {
        let object = match name {
            "profile8" => include_str!("../../tests/fixtures/mapping/profile8.json"),
            "profile84" => include_str!("../../tests/fixtures/mapping/profile84.json"),
            "mel_orig" => include_str!("../../tests/fixtures/mapping/mel_orig.json"),
            "fel_orig" => include_str!("../../tests/fixtures/mapping/fel_orig.json"),
            _ => panic!("unknown test fixture"),
        };
        format!("[{object}]")
    }

    #[test]
    fn accepts_profile8_mel_and_fel_fixtures() {
        let p8 = inspect_reader(fixture_json("profile8").as_bytes(), 8, 1).unwrap();
        assert_eq!(p8.policy, MappingPolicy::Profile8Identity);
        let mel = inspect_reader(fixture_json("mel_orig").as_bytes(), 7, 1).unwrap();
        assert_eq!(mel.stats.identity_mel, 1);
        let fel = inspect_reader(fixture_json("fel_orig").as_bytes(), 7, 1).unwrap();
        assert_eq!(fel.stats.fel, 1);
    }

    #[test]
    fn rejects_nonidentity_mmr_truncation_reuse_and_count_mismatch() {
        let mmr = inspect_reader(fixture_json("profile84").as_bytes(), 8, 1).unwrap_err();
        assert!(mmr.contains("canonical identity") || mmr.contains("MMR"));
        let json = fixture_json("profile8");
        let altered = json.replacen("\"pivots\":[0,1023]", "\"pivots\":[0,1022]", 1);
        assert!(inspect_reader(altered.as_bytes(), 8, 1)
            .unwrap_err()
            .contains("canonical identity"));
        assert!(inspect_reader(&json.as_bytes()[..json.len() - 2], 8, 1).is_err());
        let reused = json.replace(
            "\"use_prev_vdr_rpu_flag\":false",
            "\"use_prev_vdr_rpu_flag\":true",
        );
        assert!(inspect_reader(reused.as_bytes(), 8, 1)
            .unwrap_err()
            .contains("reuses"));
        assert!(inspect_reader(json.as_bytes(), 8, 2)
            .unwrap_err()
            .contains("expected 2"));
    }

    #[test]
    fn editor_modes_are_bounded() {
        assert_eq!(MappingPolicy::Profile8Identity.editor_mode(), 0);
        assert_eq!(MappingPolicy::Profile7Compatibility.editor_mode(), 2);
        assert_eq!(MappingPolicy::PreserveForSyncRepair.editor_mode(), 0);
    }

    #[test]
    fn later_nonidentity_mapping_is_rejected_with_its_frame_index() {
        let mut records: serde_json::Value =
            serde_json::from_str(&fixture_json("profile8")).unwrap();
        let mut changed = records[0].clone();
        changed["rpu_data_mapping"]["curves"][2]["poly_coef"][0][1] = 1.into();
        records.as_array_mut().unwrap().push(changed);
        let error = inspect_reader(records.to_string().as_bytes(), 8, 2).unwrap_err();
        assert!(error.contains("frame 1"), "{error}");
        assert!(error.contains("canonical identity"), "{error}");
    }

    #[test]
    fn mixed_explicit_mel_fel_keeps_per_frame_conversion_policy() {
        let mel: serde_json::Value = serde_json::from_str(&fixture_json("mel_orig")).unwrap();
        let fel: serde_json::Value = serde_json::from_str(&fixture_json("fel_orig")).unwrap();
        let records = serde_json::json!([mel[0], fel[0], mel[0]]);
        let result = inspect_reader(records.to_string().as_bytes(), 7, 3).unwrap();
        assert_eq!(result.policy, MappingPolicy::Profile7Compatibility);
        assert_eq!(result.stats.identity_mel, 2);
        assert_eq!(result.stats.fel, 1);
    }

    #[test]
    fn missing_and_unknown_representations_cannot_become_identity() {
        let original: serde_json::Value = serde_json::from_str(&fixture_json("profile8")).unwrap();
        for case in 0..7 {
            let mut records = original.clone();
            match case {
                0 => {
                    records[0]["header"]["unknown_transform"] = true.into();
                }
                1 => {
                    records[0]["rpu_data_mapping"]["unknown_transform"] = true.into();
                }
                2 => {
                    records[0]["rpu_data_mapping"]["curves"][0]["unknown_transform"] = true.into();
                }
                3 => {
                    records[0]["header"]["coefficient_data_type"] = 1.into();
                }
                4 => {
                    records[0]
                        .as_object_mut()
                        .unwrap()
                        .remove("rpu_data_mapping");
                }
                5 => {
                    records[0]["header"]["bl_bit_depth_minus8"] = 4.into();
                }
                6 => {
                    records[0]["rpu_data_mapping"]["curves"][0]["linear_interp_flag"][0] =
                        true.into();
                }
                _ => unreachable!(),
            }
            assert!(
                inspect_reader(records.to_string().as_bytes(), 8, 1).is_err(),
                "case {case}"
            );
        }
    }
}
