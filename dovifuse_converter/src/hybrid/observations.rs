//! Per-job measurement observations shared by the hybrid grade and
//! active-area checks.
//!
//! Crop detection is the expensive part that the two checks can legitimately
//! share.  The cache is deliberately scoped to one hybrid job and only keeps
//! successful rectangles.  Decode failures and a successful run with no
//! detected rectangle are retried so a missing observation cannot become
//! sticky evidence.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::exec::AppResult;
use crate::ffmpeg::{cropdetect_filter, cropdetect_window, CropRect, SampleWindow};
use crate::logger::Logger;
use crate::runtime::Runtime;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct CropObservationKey {
    file: PathBuf,
    start_s: String,
    duration_s: String,
    filter: String,
}

impl CropObservationKey {
    fn new(file: &Path, window: &SampleWindow, filter: &str) -> Option<Self> {
        if !window.start_s.is_finite()
            || window.start_s < 0.0
            || !window.dur_s.is_finite()
            || window.dur_s <= 0.0
        {
            return None;
        }
        // These are the exact decimal arguments emitted by cropdetect_window.
        // Quantizing the key to that precision lets independently generated
        // CFR windows reuse the same ffmpeg request without treating merely
        // equivalent floating point values as distinct work.
        Some(Self {
            file: file.to_path_buf(),
            start_s: format!("{:.9}", window.start_s),
            duration_s: format!("{:.9}", window.dur_s),
            filter: filter.to_string(),
        })
    }
}

#[derive(Default)]
pub(crate) struct MeasurementObservations {
    crops: HashMap<CropObservationKey, CropRect>,
}

impl MeasurementObservations {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Run or reuse one cropdetect request for this hybrid job.
    ///
    /// The cache stores only `Some(CropRect)`.  `Ok(None)` and every error are
    /// deliberately left uncached because they do not establish reusable
    /// picture-area evidence.
    pub(crate) fn cropdetect(
        &mut self,
        rt: &Runtime,
        logger: &Logger,
        file: &Path,
        window: &SampleWindow,
        limit: f64,
    ) -> AppResult<Option<CropRect>> {
        let filter = cropdetect_filter(limit);
        let Some(key) = CropObservationKey::new(file, window, &filter) else {
            return cropdetect_window(rt, logger, file, window, limit);
        };
        self.measure_crop(key, || cropdetect_window(rt, logger, file, window, limit))
    }

    fn measure_crop<F>(
        &mut self,
        key: CropObservationKey,
        measure: F,
    ) -> AppResult<Option<CropRect>>
    where
        F: FnOnce() -> AppResult<Option<CropRect>>,
    {
        if let Some(crop) = self.crops.get(&key).copied() {
            return Ok(Some(crop));
        }
        let crop = measure()?;
        if let Some(rectangle) = crop {
            self.crops.insert(key, rectangle);
        }
        Ok(crop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(start_s: f64, duration_s: f64, filter: &str) -> CropObservationKey {
        CropObservationKey::new(
            Path::new("/tmp/target.mkv"),
            &SampleWindow {
                start_s,
                dur_s: duration_s,
            },
            filter,
        )
        .unwrap()
    }

    #[test]
    fn reuses_only_exact_request_and_does_not_cache_errors() {
        let mut observations = MeasurementObservations::new();
        let rectangle = CropRect {
            w: 3840,
            h: 1600,
            x: 0,
            y: 280,
        };
        let mut calls = 0;
        let request = key(
            12.345678901_1,
            5.000000000_1,
            "cropdetect=limit=0.08:round=2:reset=0",
        );
        let equivalent_request = key(
            12.345678901_2,
            5.000000000_2,
            "cropdetect=limit=0.08:round=2:reset=0",
        );
        assert_eq!(request, equivalent_request);
        assert_eq!(
            observations
                .measure_crop(request.clone(), || {
                    calls += 1;
                    Ok(Some(rectangle))
                })
                .unwrap(),
            Some(rectangle)
        );
        assert_eq!(
            observations
                .measure_crop(equivalent_request, || {
                    calls += 1;
                    Err("cached request should not run".into())
                })
                .unwrap(),
            Some(rectangle)
        );
        assert_eq!(calls, 1);

        for changed_request in [
            key(12.345678902, 5.0, "cropdetect=limit=0.08:round=2:reset=0"),
            key(12.345678901, 5.0, "cropdetect=limit=0.10:round=2:reset=0"),
        ] {
            assert!(observations
                .measure_crop(changed_request, || {
                    calls += 1;
                    Ok(None)
                })
                .unwrap()
                .is_none());
        }
        assert_eq!(calls, 3);

        let failed_request = key(40.0, 5.0, "cropdetect=limit=0.08:round=2:reset=0");
        assert!(observations
            .measure_crop(failed_request.clone(), || {
                calls += 1;
                Err("transient ffmpeg failure".into())
            })
            .is_err());
        assert_eq!(
            observations
                .measure_crop(failed_request, || {
                    calls += 1;
                    Ok(Some(rectangle))
                })
                .unwrap(),
            Some(rectangle)
        );
        assert_eq!(calls, 5);
    }
}
