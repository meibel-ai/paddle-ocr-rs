/// How detection input dimensions are snapped to a multiple of 32.
///
/// The detector's stride requires a multiple of 32, but *which* multiple you
/// pick is a fidelity question, not a formality. The two axes snap
/// independently, so the choice also perturbs the aspect ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RoundMode {
    /// Round to the nearest multiple of 32: `((d + 16) / 32) * 32`.
    ///
    /// This is what PaddleOCR's `DetResizeForTest` does, so it is what the
    /// detector saw in training. Note it can round *up* past the source size
    /// (a 500px side becomes 512) — that is intended and matches reference.
    #[default]
    Nearest,
    /// Truncate to a multiple of 32: `(d / 32) * 32`.
    ///
    /// This crate's historical behaviour, kept only so callers can reproduce
    /// output from before [`RoundMode::Nearest`] existed. It shrinks each axis
    /// by up to 31px — an uncapped 1144x1500 page is fed as 1120x1472, a ~2%
    /// downscale with a distorted aspect ratio, neither of which the detector
    /// was trained on. Prefer `Nearest`.
    Floor,
}

impl RoundMode {
    /// Snap one dimension, never below 32.
    fn apply(self, v: f32) -> u32 {
        let snapped = match self {
            Self::Nearest => ((v / 32.0).round() as u32) * 32,
            Self::Floor => ((v as u32) / 32) * 32,
        };
        snapped.max(32)
    }
}

#[derive(Debug)]
pub struct ScaleParam {
    pub src_width: u32,
    pub src_height: u32,
    pub dst_width: u32,
    pub dst_height: u32,
    pub scale_width: f32,
    pub scale_height: f32,
}

impl ScaleParam {
    pub fn new(
        src_width: u32,
        src_height: u32,
        dst_width: u32,
        dst_height: u32,
        scale_width: f32,
        scale_height: f32,
    ) -> Self {
        Self {
            src_width,
            src_height,
            dst_width,
            dst_height,
            scale_width,
            scale_height,
        }
    }

    /// Legacy entry point: truncating round, for bit-compatibility with output
    /// produced before [`RoundMode`] existed. Prefer
    /// [`Self::get_scale_param_with_rounding`] with [`RoundMode::Nearest`].
    pub fn get_scale_param(src: &image::RgbImage, target_size: u32) -> Self {
        Self::get_scale_param_with_rounding(src, target_size, RoundMode::Floor)
    }

    /// Compute the detection resize, snapping both axes per `round_mode`.
    pub fn get_scale_param_with_rounding(
        src: &image::RgbImage,
        target_size: u32,
        round_mode: RoundMode,
    ) -> Self {
        let src_width = src.width();
        let src_height = src.height();

        let ratio: f32 = if src_width > src_height {
            target_size as f32 / src_width as f32
        } else {
            target_size as f32 / src_height as f32
        };

        let dst_width = round_mode.apply(src_width as f32 * ratio);
        let dst_height = round_mode.apply(src_height as f32 * ratio);

        let scale_width = dst_width as f32 / src_width as f32;
        let scale_height = dst_height as f32 / src_height as f32;

        Self::new(
            src_width,
            src_height,
            dst_width,
            dst_height,
            scale_width,
            scale_height,
        )
    }
}

impl std::fmt::Display for ScaleParam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "src_width:{},src_height:{},dst_width:{},dst_height:{},scale_width:{},scale_height:{}",
            self.src_width,
            self.src_height,
            self.dst_width,
            self.dst_height,
            self.scale_width,
            self.scale_height
        )
    }
}

#[cfg(test)]
mod round_mode_tests {
    use super::{RoundMode, ScaleParam};

    fn img(w: u32, h: u32) -> image::RgbImage {
        image::RgbImage::new(w, h)
    }

    /// The defect this mode exists to fix. PaddleOCR feeds a 1144x1500 page at
    /// 1152x1504; the truncating mode fed it at 1120x1472 — a ~2% downscale on
    /// a detector that is not scale-invariant.
    #[test]
    fn nearest_matches_paddleocr_where_floor_shrinks() {
        let src = img(1144, 1500);
        let near = ScaleParam::get_scale_param_with_rounding(&src, 1500, RoundMode::Nearest);
        let floor = ScaleParam::get_scale_param_with_rounding(&src, 1500, RoundMode::Floor);
        assert_eq!((near.dst_width, near.dst_height), (1152, 1504));
        assert_eq!((floor.dst_width, floor.dst_height), (1120, 1472));
    }

    /// Rounding up past the source size is intended: PaddleOCR does it, and a
    /// stride-aligned input is what the model was trained on.
    #[test]
    fn nearest_may_round_up_past_the_source() {
        let p = ScaleParam::get_scale_param_with_rounding(&img(640, 500), 640, RoundMode::Nearest);
        assert_eq!((p.dst_width, p.dst_height), (640, 512));
    }

    /// Both modes must stay on the stride and never collapse a thin strip to
    /// zero — a 0-width resize is a panic inside `image::imageops::resize`.
    #[test]
    fn both_modes_stay_on_stride_and_never_reach_zero() {
        for mode in [RoundMode::Nearest, RoundMode::Floor] {
            for (w, h) in [(10, 10), (1, 4000), (33, 31), (4000, 3), (1, 1)] {
                let p = ScaleParam::get_scale_param_with_rounding(&img(w, h), 960, mode);
                assert_eq!(p.dst_width % 32, 0, "{mode:?} {w}x{h} width off-stride");
                assert_eq!(p.dst_height % 32, 0, "{mode:?} {w}x{h} height off-stride");
                assert!(p.dst_width >= 32 && p.dst_height >= 32, "{mode:?} {w}x{h} collapsed");
            }
        }
    }

    /// `get_scale_param` is the pre-`RoundMode` entry point; callers relying on
    /// it to reproduce old output must keep getting the truncating behaviour.
    #[test]
    fn legacy_entry_point_still_truncates() {
        let src = img(1144, 1500);
        let legacy = ScaleParam::get_scale_param(&src, 1500);
        let floor = ScaleParam::get_scale_param_with_rounding(&src, 1500, RoundMode::Floor);
        assert_eq!((legacy.dst_width, legacy.dst_height), (floor.dst_width, floor.dst_height));
    }

    /// Default must be the reference mode, so a caller who does not think about
    /// it gets PaddleOCR's behaviour rather than this crate's history.
    #[test]
    fn default_is_nearest() {
        assert_eq!(RoundMode::default(), RoundMode::Nearest);
    }
}
