//! Compare parsed metadata after all intended edits with the muxed output.
//! JSON objects have deterministic key ordering; arrays retain their order.
//! Only the encoding checksum is excluded. No creative block is excluded.

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use serde::de::{self, SeqAccess, Visitor};
use serde::Deserializer;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::exec::{run_status, AppResult};
use crate::logger::Logger;
use crate::runtime::Runtime;

pub(crate) struct Metadata(Vec<[u8; 32]>);

pub(crate) fn capture(
    rpu: &Path,
    export: &Path,
    frames: u64,
    target_l6: Option<&Value>,
    rt: &Runtime,
    logger: &Logger,
) -> AppResult<Metadata> {
    run_status(
        logger,
        rt.dry_run,
        true,
        &rt.dovi_tool,
        &[
            OsString::from("export"),
            OsString::from("-i"),
            rpu.as_os_str().to_owned(),
            OsString::from("-d"),
            OsString::from(format!("all={}", export.display())),
        ],
    )?;
    let file = File::open(export).map_err(|e| format!("Metadata transport export: {e}"))?;
    read_metadata(BufReader::new(file), frames, target_l6)
}

pub(crate) fn verify(expected: &Metadata, actual: &Metadata) -> AppResult<()> {
    if expected.0.len() != actual.0.len() {
        return Err("Output metadata transport frame count differs".into());
    }
    if let Some(frame) = expected.0.iter().zip(&actual.0).position(|(a, b)| a != b) {
        return Err(format!(
            "Output metadata transport differs from the edited RPU at frame {frame}"
        ));
    }
    Ok(())
}

struct MetadataVisitor<'a> {
    frames: u64,
    target_l6: Option<&'a Value>,
}

impl<'de> Visitor<'de> for MetadataVisitor<'_> {
    type Value = Metadata;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("an array containing every parsed RPU")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Metadata, A::Error> {
        let mut hashes = Vec::new();
        while let Some(mut rpu) = seq.next_element::<Value>()? {
            if crate::cancellation::requested() {
                return Err(de::Error::custom("Conversion cancelled"));
            }
            let frame = hashes.len() as u64;
            if frame >= self.frames {
                return Err(de::Error::custom(
                    "Metadata transport export has extra frames",
                ));
            }
            let object = rpu
                .as_object_mut()
                .ok_or_else(|| de::Error::custom("Invalid RPU object"))?;
            if !object.contains_key("header") || !object.contains_key("dovi_profile") {
                return Err(de::Error::custom("Missing parsed RPU header/profile"));
            }
            object.remove("rpu_data_crc32");
            if let Some(expected) = self.target_l6 {
                let blocks = rpu
                    .pointer("/vdr_dm_data/cmv29_metadata/ext_metadata_blocks")
                    .and_then(Value::as_array);
                let levels: Vec<_> = blocks
                    .into_iter()
                    .flatten()
                    .filter_map(|b| b.get("Level6"))
                    .collect();
                if levels.as_slice() != [expected] {
                    return Err(de::Error::custom(format!(
                        "Edited RPU L6 differs from the target plan at frame {frame}"
                    )));
                }
            }
            let bytes = serde_json::to_vec(&rpu).map_err(de::Error::custom)?;
            hashes.push(Sha256::digest(bytes).into());
        }
        if hashes.len() as u64 != self.frames {
            return Err(de::Error::custom(format!(
                "Metadata transport export has {} frames; expected {}",
                hashes.len(),
                self.frames
            )));
        }
        Ok(Metadata(hashes))
    }
}

fn read_metadata<R: Read>(
    reader: R,
    frames: u64,
    target_l6: Option<&Value>,
) -> AppResult<Metadata> {
    if frames == 0 {
        return Err("Metadata transport requires a nonzero frame count".into());
    }
    let mut decoder = serde_json::Deserializer::from_reader(reader);
    let result = decoder
        .deserialize_seq(MetadataVisitor { frames, target_l6 })
        .map_err(|e| e.to_string())?;
    decoder.end().map_err(|e| e.to_string())?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parsed(value: Value, frames: u64) -> Metadata {
        read_metadata(serde_json::to_vec(&value).unwrap().as_slice(), frames, None).unwrap()
    }

    #[test]
    fn ignores_only_crc_and_object_key_order() {
        let a = parsed(
            json!([{"header":{}, "dovi_profile":8, "rpu_data_crc32":1}]),
            1,
        );
        let b = parsed(
            json!([{"rpu_data_crc32":2, "dovi_profile":8, "header":{}}]),
            1,
        );
        assert!(verify(&a, &b).is_ok());
    }

    #[test]
    fn catches_late_changes_to_any_block() {
        let frame = json!({"header":{}, "dovi_profile":8, "vdr_dm_data":{"blocks":[1,2]}});
        let a = parsed(json!([frame, frame]), 2);
        let mut changed = frame.clone();
        changed["vdr_dm_data"]["blocks"] = json!([2, 1]);
        let b = parsed(json!([frame, changed]), 2);
        assert!(verify(&a, &b).unwrap_err().contains("frame 1"));
    }

    #[test]
    fn rejects_incomplete_or_malformed_exports() {
        for input in [
            "[]",
            "[{}]",
            "[null]",
            "[{\"header\":{},\"dovi_profile\":8}] trailing",
        ] {
            assert!(read_metadata(input.as_bytes(), 1, None).is_err());
        }
    }

    #[test]
    fn validates_actual_l6_not_container_tags() {
        let l6 = json!({"min_display_mastering_luminance":0});
        let rpu = json!({"header":{},"dovi_profile":8,"vdr_dm_data":{"cmv29_metadata":{"ext_metadata_blocks":[{"Level6":l6}]}}});
        let encoded = serde_json::to_vec(&json!([rpu])).unwrap();
        assert!(read_metadata(encoded.as_slice(), 1, Some(&l6)).is_ok());
        assert!(read_metadata(
            encoded.as_slice(),
            1,
            Some(&json!({"min_display_mastering_luminance":1}))
        )
        .is_err());
    }
}
