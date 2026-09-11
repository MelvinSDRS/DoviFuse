//! Frame-addressed effective L5 offsets. Half-open ranges internally; inclusive
//! ranges only at the dovi_tool boundary. No image-measurement claims live here.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Bars {
    pub left: u32,
    pub right: u32,
    pub top: u32,
    pub bottom: u32,
}
impl Bars {
    #[cfg(test)]
    pub const ZERO: Self = Self {
        left: 0,
        right: 0,
        top: 0,
        bottom: 0,
    };
    pub fn max_edge_diff(&self, other: &Self) -> u32 {
        self.left
            .abs_diff(other.left)
            .max(self.right.abs_diff(other.right))
            .max(self.top.abs_diff(other.top))
            .max(self.bottom.abs_diff(other.bottom))
    }
    pub fn validate(&self, width: u32, height: u32) -> Result<(), String> {
        if width == 0
            || height == 0
            || [self.left, self.right, self.top, self.bottom]
                .iter()
                .any(|&v| v > u16::MAX as u32)
            || self.left.checked_add(self.right).is_none_or(|v| v >= width)
            || self
                .top
                .checked_add(self.bottom)
                .is_none_or(|v| v >= height)
        {
            return Err("L5 offsets are outside the target canvas".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Interval {
    pub start: u64,
    pub end: u64,
    pub bars: Bars,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Timeline {
    pub frames: u64,
    pub intervals: Vec<Interval>,
}
impl Timeline {
    pub fn new(
        frames: u64,
        mut intervals: Vec<Interval>,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        if frames == 0 {
            return Err("Empty L5 timeline".into());
        }
        intervals.sort_by_key(|i| i.start);
        let mut cursor = 0;
        let mut normalized: Vec<Interval> = Vec::new();
        for interval in intervals {
            if interval.start != cursor || interval.end <= interval.start || interval.end > frames {
                return Err(format!(
                    "L5 timeline gap, overlap or invalid range at frame {cursor}"
                ));
            }
            interval.bars.validate(width, height)?;
            cursor = interval.end;
            if let Some(last) = normalized
                .last_mut()
                .filter(|last| last.bars == interval.bars)
            {
                last.end = interval.end;
            } else {
                normalized.push(interval);
            }
        }
        if cursor != frames {
            return Err(format!("L5 timeline covers {cursor} of {frames} frames"));
        }
        Ok(Self {
            frames,
            intervals: normalized,
        })
    }
    pub fn constant(frames: u64, bars: Bars, width: u32, height: u32) -> Result<Self, String> {
        Self::new(
            frames,
            vec![Interval {
                start: 0,
                end: frames,
                bars,
            }],
            width,
            height,
        )
    }
    pub fn from_export(text: &str, frames: u64, width: u32, height: u32) -> Result<Self, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Preset {
            id: u16,
            left: u32,
            right: u32,
            top: u32,
            bottom: u32,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Export {
            crop: bool,
            presets: Vec<Preset>,
            edits: BTreeMap<String, u16>,
        }
        let export: Export =
            serde_json::from_str(text).map_err(|e| format!("Invalid L5 export: {e}"))?;
        if !export.crop {
            return Err("Unexpected L5 export crop flag".into());
        }
        let mut presets = BTreeMap::new();
        for p in export.presets {
            let bars = Bars {
                left: p.left,
                right: p.right,
                top: p.top,
                bottom: p.bottom,
            };
            bars.validate(width, height)?;
            if presets.insert(p.id, bars).is_some() {
                return Err("Duplicate L5 preset ID".into());
            }
        }
        let mut intervals = Vec::new();
        for (range, id) in export.edits {
            let bars = *presets.get(&id).ok_or("Unknown L5 preset ID")?;
            let (start, end) = range
                .split_once('-')
                .ok_or("L5 export requires explicit frame ranges")?;
            let start = start.parse::<u64>().map_err(|_| "Invalid L5 start frame")?;
            let end = end
                .parse::<u64>()
                .ok()
                .and_then(|v| v.checked_add(1))
                .ok_or("Invalid L5 end frame")?;
            intervals.push(Interval { start, end, bars });
        }
        Self::new(frames, intervals, width, height)
    }
    /// This config must be applied AFTER trimming/duplication, in a separate pass.
    pub fn editor_config(&self) -> Value {
        let mut presets = Vec::<Bars>::new();
        let mut edits = BTreeMap::new();
        for i in &self.intervals {
            let id = if let Some(id) = presets.iter().position(|b| *b == i.bars) {
                id
            } else {
                presets.push(i.bars);
                presets.len() - 1
            };
            edits.insert(format!("{}-{}", i.start, i.end - 1), id);
        }
        json!({"mode":0,"remove_mapping":false,"remove_cmv4":false,"active_area":{
            "crop":true,"presets":presets.iter().enumerate().map(|(id,b)|json!({"id":id,"left":b.left,"right":b.right,"top":b.top,"bottom":b.bottom})).collect::<Vec<_>>(),"edits":edits}})
    }
    pub fn verify_export(&self, text: &str, width: u32, height: u32) -> Result<(), String> {
        let actual = Self::from_export(text, self.frames, width, height)?;
        let (mut expected_index, mut actual_index, mut frame) = (0, 0, 0);
        while frame < self.frames {
            let expected = &self.intervals[expected_index];
            let observed = &actual.intervals[actual_index];
            if expected.bars != observed.bars {
                return Err(format!(
                    "Output L5 differs from expected timeline at frame {frame}"
                ));
            }
            frame = expected.end.min(observed.end);
            if expected.end == frame {
                expected_index += 1;
            }
            if observed.end == frame {
                actual_index += 1;
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> Bars {
        Bars {
            top: 20,
            bottom: 20,
            ..Bars::ZERO
        }
    }
    #[test]
    fn variable_timeline_roundtrips_with_inclusive_editor_boundaries() {
        let t = Timeline::new(
            30,
            vec![
                Interval {
                    start: 0,
                    end: 10,
                    bars: scope(),
                },
                Interval {
                    start: 10,
                    end: 20,
                    bars: Bars::ZERO,
                },
                Interval {
                    start: 20,
                    end: 30,
                    bars: scope(),
                },
            ],
            320,
            180,
        )
        .unwrap();
        let config = t.editor_config();
        assert_eq!(config["active_area"]["edits"]["10-19"], 1);
        t.verify_export(&config["active_area"].to_string(), 320, 180)
            .unwrap();
        let mut shifted = config["active_area"].clone();
        shifted["edits"] = json!({"0-10":0,"11-19":1,"20-29":0});
        assert!(t
            .verify_export(&shifted.to_string(), 320, 180)
            .unwrap_err()
            .contains("frame 10"));
    }
    #[test]
    fn gaps_overlaps_truncation_unknown_presets_and_overflow_are_rejected() {
        let t = Timeline::constant(30, scope(), 320, 180).unwrap();
        for edits in [
            json!({"1-29":0}),
            json!({"0-20":0,"20-29":0}),
            json!({"0-28":0}),
            json!({"0-29":7}),
            json!({"0-18446744073709551615":0}),
        ] {
            let mut config = t.editor_config()["active_area"].clone();
            config["edits"] = edits;
            assert!(Timeline::from_export(&config.to_string(), 30, 320, 180).is_err());
        }
        let mut config = t.editor_config()["active_area"].clone();
        config["presets"][0]["left"] = json!(4294967296u64);
        assert!(Timeline::from_export(&config.to_string(), 30, 320, 180).is_err());
    }
    #[test]
    fn equivalent_segmentations_and_preset_ids_are_accepted() {
        let t = Timeline::constant(30, scope(), 320, 180).unwrap();
        let mut config = t.editor_config()["active_area"].clone();
        config["presets"][0]["id"] = json!(5);
        config["edits"] = json!({"0-12":5,"13-29":5});
        t.verify_export(&config.to_string(), 320, 180).unwrap();
    }
    #[test]
    fn invalid_canvas_or_offsets_fail_instead_of_saturating() {
        assert!(Timeline::constant(
            3,
            Bars {
                left: u32::MAX,
                ..Bars::ZERO
            },
            320,
            180
        )
        .is_err());
        assert!(Timeline::constant(
            3,
            Bars {
                top: 100,
                bottom: 80,
                ..Bars::ZERO
            },
            320,
            180
        )
        .is_err());
        assert!(Timeline::constant(0, Bars::ZERO, 320, 180).is_err());
    }
}
