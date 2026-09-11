//! Preserve container-level video metadata when replacing its elementary stream.
use crate::exec::{run_capture, AppResult};
use crate::logger::Logger;
use crate::runtime::Runtime;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::Path;
use unicode_normalization::UnicodeNormalization;

const FLAGS: &[(&str, &str)] = &[
    ("default_track", "--default-track-flag"),
    ("forced_track", "--forced-display-flag"),
    ("enabled_track", "--track-enabled-flag"),
    ("flag_hearing_impaired", "--hearing-impaired-flag"),
    ("flag_visual_impaired", "--visual-impaired-flag"),
    ("flag_text_descriptions", "--text-descriptions-flag"),
    ("flag_original", "--original-flag"),
    ("flag_commentary", "--commentary-flag"),
];
const TRACK_FIELDS: &[&str] = &[
    "codec_id",
    "language",
    "language_ietf",
    "track_name",
    "default_track",
    "forced_track",
    "enabled_track",
    "flag_hearing_impaired",
    "flag_visual_impaired",
    "flag_text_descriptions",
    "flag_original",
    "flag_commentary",
];
const VIDEO_FIELDS: &[&str] = &[
    "display_dimensions",
    "display_unit",
    "pixel_dimensions",
    "stereo_mode",
];
const OTHER_FIELDS: &[&str] = &[
    "codec_private_data",
    "audio_channels",
    "audio_sampling_frequency",
    "audio_bits_per_sample",
    "uid",
];

fn identify(path: &Path, rt: &Runtime, logger: &Logger) -> AppResult<Value> {
    let out = run_capture(
        logger,
        &rt.mkvmerge,
        &["-J".into(), path.as_os_str().to_owned()],
    )?;
    serde_json::from_str(&out).map_err(|e| format!("Invalid container manifest: {e}"))
}
fn tracks(manifest: &Value) -> AppResult<&Vec<Value>> {
    manifest["tracks"]
        .as_array()
        .ok_or_else(|| "Container has no track manifest".into())
}

pub(crate) struct RemuxSource {
    manifest: Value,
    video: Value,
}
impl RemuxSource {
    pub(crate) fn read(path: &Path, rt: &Runtime, logger: &Logger) -> AppResult<Self> {
        Self::from_manifest(identify(path, rt, logger)?)
    }
    fn from_manifest(manifest: Value) -> AppResult<Self> {
        let videos: Vec<_> = tracks(&manifest)?
            .iter()
            .filter(|t| t["type"] == "video")
            .collect();
        if videos.len() != 1 {
            return Err("Exactly one video track is required for remux preservation".into());
        }
        let video = videos[0]["properties"].clone();
        if video["display_unit"].as_u64().is_some_and(|v| v != 0) {
            return Err(
                "Non-pixel display units cannot yet be preserved; keeping the source".into(),
            );
        }
        if video["stereo_mode"].as_u64().is_some_and(|v| v > 14) {
            return Err("Unsupported stereo metadata; keeping the source".into());
        }
        for field in ["display_dimensions", "pixel_dimensions"] {
            if let Some(v) = video[field].as_str() {
                let valid = v.split_once('x').is_some_and(|(w, h)| {
                    w.parse::<u32>().is_ok_and(|v| v > 0) && h.parse::<u32>().is_ok_and(|v| v > 0)
                });
                if !valid {
                    return Err(format!("Invalid {field} in source video"));
                }
            }
        }
        for (key, _) in FLAGS {
            if !video[*key].is_null() && !video[*key].is_boolean() {
                return Err(format!("Invalid video flag {key}"));
            }
        }
        Ok(Self { manifest, video })
    }
    pub(crate) fn arguments(
        &self,
        source: &Path,
        hevc: &Path,
        timestamps: &Path,
        output: &Path,
    ) -> AppResult<Vec<OsString>> {
        let mut args: Vec<OsString> = vec![
            "-o".into(),
            output.as_os_str().to_owned(),
            "--normalize-language-ietf".into(),
            "off".into(),
            "-D".into(),
            source.as_os_str().to_owned(),
        ];
        let mut option = |name: &str, value: String| {
            args.push(name.into());
            args.push(format!("0:{value}").into());
        };
        if let Some(language) = self.video["language_ietf"]
            .as_str()
            .or_else(|| self.video["language"].as_str())
        {
            option("--language", language.to_string());
        }
        if let Some(name) = self.video["track_name"].as_str() {
            option("--track-name", name.to_string());
        }
        for (key, flag) in FLAGS {
            if let Some(value) = self.video[*key].as_bool() {
                option(flag, if value { "yes" } else { "no" }.into());
            }
        }
        if let Some(dimensions) = self.video["display_dimensions"].as_str() {
            option("--display-dimensions", dimensions.into());
        }
        if let Some(stereo) = self.video["stereo_mode"].as_u64() {
            option("--stereo-mode", stereo.to_string());
        }
        option("--timestamps", timestamps.to_string_lossy().into_owned());
        args.push(hevc.as_os_str().to_owned());
        let order = tracks(&self.manifest)?
            .iter()
            .map(|track| {
                if track["type"] == "video" {
                    Ok("1:0".to_string())
                } else {
                    track["id"]
                        .as_u64()
                        .map(|id| format!("0:{id}"))
                        .ok_or_else(|| "Missing source track ID".to_string())
                }
            })
            .collect::<AppResult<Vec<_>>>()?
            .join(",");
        args.extend(["--track-order".into(), order.into()]);
        Ok(args)
    }
    pub(crate) fn verify(&self, output: &Path, rt: &Runtime, logger: &Logger) -> AppResult<()> {
        self.verify_manifest(&identify(output, rt, logger)?)?;
        if self.regenerates_track_uids() {
            logger.ok("MakeMKV output track identifiers verified (nonzero and unique)");
        }
        logger
            .ok("Container track settings, order, chapter counts and attachment metadata verified");
        Ok(())
    }
    fn regenerates_track_uids(&self) -> bool {
        // Match MKVToolNix's Matroska reader: MakeMKV identifiers are
        // regenerated, including corresponding chapter/tag references.
        self.manifest["container"]["properties"]["writing_application"]
            .as_str()
            .is_some_and(|app| app.to_ascii_lowercase().contains("makemkv"))
    }
    fn verify_manifest(&self, output: &Value) -> AppResult<()> {
        let a = tracks(&self.manifest)?;
        let b = tracks(output)?;
        if a.len() != b.len() {
            return Err("Remux changed the number of tracks".into());
        }
        let regenerated_uids = self.regenerates_track_uids();
        if regenerated_uids {
            let mut identifiers = HashSet::new();
            for track in b {
                let uid = track["properties"]["uid"].as_u64().unwrap_or(0);
                if uid == 0 || !identifiers.insert(uid) {
                    return Err("Remux produced missing, zero or duplicate track UIDs".into());
                }
            }
        }
        for (index, (source, target)) in a.iter().zip(b).enumerate() {
            if source["type"] != target["type"] {
                return Err(format!("Remux changed track order at {index}"));
            }
            let video = source["type"] == "video";
            let extra = if video { VIDEO_FIELDS } else { OTHER_FIELDS };
            for key in TRACK_FIELDS.iter().chain(extra) {
                let before = &source["properties"][*key];
                let after = &target["properties"][*key];
                // Absent optional flags are false; mkvmerge may make them explicit.
                let equivalent = if *key == "uid" && regenerated_uids {
                    // The output UIDs were validated above. Requiring numeric
                    // equality here would reject every normal MakeMKV remux.
                    true
                } else if FLAGS.iter().any(|(flag, _)| flag == key) {
                    before.as_bool().unwrap_or(false) == after.as_bool().unwrap_or(false)
                } else if *key == "stereo_mode" || *key == "display_unit" {
                    before.as_u64().unwrap_or(0) == after.as_u64().unwrap_or(0)
                } else if *key == "track_name" {
                    // MKVToolNix on macOS decomposes accents in option strings.
                    // Accept only canonical equivalence, preserving case and wording.
                    before
                        .as_str()
                        .unwrap_or("")
                        .nfc()
                        .eq(after.as_str().unwrap_or("").nfc())
                } else {
                    before.is_null() || before == after
                };
                if !equivalent {
                    return Err(format!(
                        "Remux changed track {index} property {key}: {before} -> {after}"
                    ));
                }
            }
        }
        let attachments = |m: &Value| {
            m["attachments"].as_array().map(|a|a.iter().map(|v|json!({"name":v["file_name"],"size":v["size"],"type":v["content_type"],"description":v["description"]})).collect::<Vec<_>>()).unwrap_or_default()
        };
        if attachments(&self.manifest) != attachments(output) {
            return Err("Remux changed attachment metadata".into());
        }
        if self.manifest["chapters"] != output["chapters"] {
            return Err("Remux changed chapter counts".into());
        }
        if self.manifest["container"]["properties"]["title"]
            != output["container"]["properties"]["title"]
        {
            return Err("Remux changed the container title".into());
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        json!({"tracks":[{"id":0,"type":"audio","properties":{"codec_id":"A_AAC","uid":12}},
        {"id":1,"type":"video","properties":{"codec_id":"V_MPEGH/ISO/HEVC","language":"fre","language_ietf":"fr-CA","track_name":"Image cinéma","default_track":false,"forced_track":true,"display_dimensions":"1920x1080","display_unit":0,"stereo_mode":1}}],"chapters":[{"num_entries":2}],"attachments":[]})
    }
    #[test]
    fn video_metadata_is_applied_to_replacement_and_order_is_preserved() {
        let source = RemuxSource::from_manifest(fixture()).unwrap();
        let args = source
            .arguments(
                Path::new("source.mkv"),
                Path::new("new.hevc"),
                Path::new("times.txt"),
                Path::new("out.mkv"),
            )
            .unwrap();
        assert!(args
            .windows(2)
            .any(|w| w == [OsString::from("--language"), OsString::from("0:fr-CA")]));
        assert!(args
            .windows(2)
            .any(|w| w == [OsString::from("--track-order"), OsString::from("0:0,1:0")]));
        assert!(args.windows(2).any(|w| w
            == [
                OsString::from("--default-track-flag"),
                OsString::from("0:no")
            ]));
    }
    #[test]
    fn changed_header_or_lost_track_is_rejected() {
        let source = RemuxSource::from_manifest(fixture()).unwrap();
        let mut out = fixture();
        out["tracks"][1]["properties"]["language_ietf"] = json!("en");
        assert!(source.verify_manifest(&out).is_err());
        let mut out = fixture();
        out["tracks"].as_array_mut().unwrap().pop();
        assert!(source.verify_manifest(&out).is_err());
    }
    #[test]
    fn makemkv_uid_regeneration_is_expected_but_invalid_identifiers_are_rejected() {
        let mut input = fixture();
        input["container"]["properties"]["writing_application"] = json!("MakeMKV v1.15.3");
        let source = RemuxSource::from_manifest(input.clone()).unwrap();
        let mut output = input;
        output["tracks"][0]["properties"]["uid"] = json!(100);
        output["tracks"][1]["properties"]["uid"] = json!(200);
        assert!(source.verify_manifest(&output).is_ok());
        for invalid in [Value::Null, json!(0), json!(100)] {
            output["tracks"][1]["properties"]["uid"] = invalid;
            assert!(source.verify_manifest(&output).is_err());
        }
        let ordinary = RemuxSource::from_manifest(fixture()).unwrap();
        let mut changed = fixture();
        changed["tracks"][0]["properties"]["uid"] = json!(100);
        assert!(ordinary.verify_manifest(&changed).is_err());
    }
    #[test]
    fn canonical_unicode_name_is_preserved_but_other_changes_fail() {
        let source = RemuxSource::from_manifest(fixture()).unwrap();
        let mut out = fixture();
        out["tracks"][1]["properties"]["track_name"] = json!("Image cine\u{301}ma");
        assert!(source.verify_manifest(&out).is_ok());
        for name in ["Image cinema", "Image Cinéma", "Image cinéma ", "Other"] {
            out["tracks"][1]["properties"]["track_name"] = json!(name);
            assert!(source.verify_manifest(&out).is_err());
        }
    }
    #[test]
    fn unsupported_geometry_is_not_silently_rewritten() {
        let mut source = fixture();
        source["tracks"][1]["properties"]["display_unit"] = json!(3);
        assert!(RemuxSource::from_manifest(source).is_err());
    }
}
