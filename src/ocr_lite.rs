use std::collections::HashMap;

use image::ImageBuffer;
use ort::session::builder::SessionBuilder;

use crate::{
    angle_net::AngleNet,
    base_net::BaseNet,
    crnn_net::{CrnnNet, RecBatchOptions},
    db_net::{DbNet, PostProcessOptions},
    ocr_error::OcrError,
    ocr_result::{OcrResult, Point, TextBlock},
    ocr_utils::OcrUtils,
    scale_param::{RoundMode, ScaleParam},
};

/// Every knob the detect → recognise pipeline takes.
///
/// This exists so callers configure the pipeline through one named value
/// instead of a twelve-argument positional call, where `box_score_thresh` and
/// `box_thresh` sit adjacent and are trivially transposed.
#[derive(Debug, Clone, Copy)]
pub struct DetectOptions {
    /// Border added around the page before detection, to improve edge recall.
    pub padding: u32,
    /// Cap on the resized long side. `0` means "do not cap" — the source size
    /// is used. This never *up*scales; it is a ceiling only.
    pub max_side_len: u32,
    /// PaddleOCR's `box_thresh`: minimum contour score to keep a box.
    pub box_score_thresh: f32,
    /// PaddleOCR's `thresh`: probability-map binarization threshold.
    pub box_thresh: f32,
    /// PaddleOCR's `unclip_ratio`: how far to expand each box.
    pub un_clip_ratio: f32,
    /// Run the angle classifier on each crop.
    pub do_angle: bool,
    /// Apply the page's majority angle to every crop.
    pub most_angle: bool,
    /// How detection input dimensions snap to a multiple of 32.
    pub round_mode: RoundMode,
    /// DB post-processing options.
    pub post: PostProcessOptions,
    /// Recognition batching options.
    pub rec: RecBatchOptions,
    /// Undo a crop's angle correction when recognition scores badly.
    pub angle_rollback: bool,
    /// Score below which `angle_rollback` triggers.
    pub angle_rollback_threshold: f32,
}

impl Default for DetectOptions {
    /// Reference PP-OCRv6: the three DB thresholds are the published values
    /// from `PP-OCRv6_medium_det_onnx/inference.yml`, and the resize/dilation
    /// behaviour matches PaddleOCR rather than this crate's history.
    fn default() -> Self {
        Self {
            padding: 50,
            max_side_len: 0,
            box_score_thresh: 0.45,
            box_thresh: 0.2,
            un_clip_ratio: 1.4,
            do_angle: false,
            most_angle: false,
            round_mode: RoundMode::Nearest,
            post: PostProcessOptions::default(),
            rec: RecBatchOptions::default(),
            angle_rollback: false,
            angle_rollback_threshold: 0.0,
        }
    }
}

impl DetectOptions {
    /// This crate's historical behaviour, for reproducing earlier output:
    /// PP-OCRv5 thresholds, truncating resize, unconditional dilation,
    /// uncapped candidates, unbatched recognition.
    pub fn legacy() -> Self {
        Self {
            box_score_thresh: 0.5,
            box_thresh: 0.3,
            un_clip_ratio: 1.6,
            round_mode: RoundMode::Floor,
            post: PostProcessOptions::legacy(),
            rec: RecBatchOptions::legacy(),
            ..Self::default()
        }
    }
}

#[derive(Debug)]
pub struct OcrLite {
    db_net: DbNet,
    angle_net: AngleNet,
    crnn_net: CrnnNet,
}

impl Default for OcrLite {
    fn default() -> Self {
        Self::new()
    }
}

impl OcrLite {
    pub fn new() -> Self {
        Self {
            db_net: DbNet::new(),
            angle_net: AngleNet::new(),
            crnn_net: CrnnNet::new(),
        }
    }

    pub fn init_models(
        &mut self,
        det_path: &str,
        cls_path: &str,
        rec_path: &str,
        num_thread: usize,
    ) -> Result<(), OcrError> {
        self.db_net.init_model(det_path, num_thread, None)?;
        self.angle_net.init_model(cls_path, num_thread, None)?;
        self.crnn_net.init_model(rec_path, num_thread, None)?;
        Ok(())
    }

    pub fn init_models_with_dict(
        &mut self,
        det_path: &str,
        cls_path: &str,
        rec_path: &str,
        dict_path: &str,
        num_thread: usize,
    ) -> Result<(), OcrError> {
        self.db_net.init_model(det_path, num_thread, None)?;
        self.angle_net.init_model(cls_path, num_thread, None)?;
        self.crnn_net
            .init_model_dict_file(rec_path, num_thread, None, dict_path)?;
        Ok(())
    }

    pub fn init_models_custom(
        &mut self,
        det_path: &str,
        cls_path: &str,
        rec_path: &str,
        builder_fn: fn(SessionBuilder) -> Result<SessionBuilder, ort::Error>,
    ) -> Result<(), OcrError> {
        self.db_net.init_model(det_path, 0, Some(builder_fn))?;
        self.angle_net.init_model(cls_path, 0, Some(builder_fn))?;
        self.crnn_net.init_model(rec_path, 0, Some(builder_fn))?;
        Ok(())
    }

    pub fn init_models_custom_with_dict(
        &mut self,
        det_path: &str,
        cls_path: &str,
        rec_path: &str,
        dict_path: &str,
        builder_fn: fn(SessionBuilder) -> Result<SessionBuilder, ort::Error>,
    ) -> Result<(), OcrError> {
        self.db_net.init_model(det_path, 0, Some(builder_fn))?;
        self.angle_net.init_model(cls_path, 0, Some(builder_fn))?;
        self.crnn_net
            .init_model_dict_file(rec_path, 0, Some(builder_fn), dict_path)?;
        Ok(())
    }

    pub fn init_models_from_memory(
        &mut self,
        det_bytes: &[u8],
        cls_bytes: &[u8],
        rec_bytes: &[u8],
        num_thread: usize,
    ) -> Result<(), OcrError> {
        self.db_net
            .init_model_from_memory(det_bytes, num_thread, None)?;
        self.angle_net
            .init_model_from_memory(cls_bytes, num_thread, None)?;
        self.crnn_net
            .init_model_from_memory(rec_bytes, num_thread, None)?;
        Ok(())
    }

    pub fn init_models_from_memory_custom(
        &mut self,
        det_bytes: &[u8],
        cls_bytes: &[u8],
        rec_bytes: &[u8],
        builder_fn: fn(SessionBuilder) -> Result<SessionBuilder, ort::Error>,
    ) -> Result<(), OcrError> {
        self.db_net
            .init_model_from_memory(det_bytes, 0, Some(builder_fn))?;
        self.angle_net
            .init_model_from_memory(cls_bytes, 0, Some(builder_fn))?;
        self.crnn_net
            .init_model_from_memory(rec_bytes, 0, Some(builder_fn))?;
        Ok(())
    }

    /// Pre-run the recogniser on every shape it will use, so cuDNN's
    /// per-shape algorithm selection (~2.5s each, measured) happens at startup
    /// rather than inside the first document to hit that shape.
    pub fn warmup(&mut self, opts: &DetectOptions) -> Result<(), OcrError> {
        let (batches, widths) = CrnnNet::warmup_shapes(opts.rec);
        if widths.is_empty() {
            return Ok(());
        }
        self.crnn_net.warmup(&batches, &widths)
    }

    fn detect_base(
        &mut self,
        img_src: &image::RgbImage,
        padding: u32,
        max_side_len: u32,
        box_score_thresh: f32,
        box_thresh: f32,
        un_clip_ratio: f32,
        do_angle: bool,
        most_angle: bool,
        angle_rollback: bool,
        angle_rollback_threshold: f32,
    ) -> Result<OcrResult, OcrError> {
        // The positional constructors predate `DetectOptions` and are kept for
        // compatibility, so they must keep this crate's historical behaviour
        // (truncating resize, unconditional dilation, unbatched recognition) —
        // not the new reference defaults.
        self.detect_with_options(
            img_src,
            &DetectOptions {
                padding,
                max_side_len,
                box_score_thresh,
                box_thresh,
                un_clip_ratio,
                do_angle,
                most_angle,
                angle_rollback,
                angle_rollback_threshold,
                ..DetectOptions::legacy()
            },
        )
    }

    /// Detect and recognise text, configured through [`DetectOptions`].
    pub fn detect_with_options(
        &mut self,
        img_src: &image::RgbImage,
        opts: &DetectOptions,
    ) -> Result<OcrResult, OcrError> {
        let origin_max_side = img_src.width().max(img_src.height());
        // `max_side_len` is a ceiling, never a target: a page smaller than the
        // cap is detected at its own size rather than upscaled, because
        // interpolated pixels carry no information the detector can use.
        let mut resize = if opts.max_side_len == 0 || opts.max_side_len > origin_max_side {
            origin_max_side
        } else {
            opts.max_side_len
        };
        resize += 2 * opts.padding;

        let padding_src = OcrUtils::make_padding(img_src, opts.padding)?;
        let scale = ScaleParam::get_scale_param_with_rounding(&padding_src, resize, opts.round_mode);

        self.detect_once(&padding_src, &scale, opts)
    }

    /// 检测图片
    ///
    /// # Arguments
    ///
    /// - `&self` (`undefined`) - Describe this parameter.
    /// - `img_src` (`&image`) - 图片
    /// - `padding` (`u32`) - 变换图片时添加边框的宽度（提高检测效果）
    /// - `max_side_len` (`u32`) - 变换图片后图片宽和高保留的最大边长（超出该尺寸的图片将缩小）
    /// - `box_score_thresh` (`f32`) - 检测存在文本的区域的分值阈值
    /// - `do_angle` (`bool`) - 是否进行角度检测
    /// ```
    pub fn detect(
        &mut self,
        img_src: &image::RgbImage,
        padding: u32,
        max_side_len: u32,
        box_score_thresh: f32,
        box_thresh: f32,
        un_clip_ratio: f32,
        do_angle: bool,
        most_angle: bool,
    ) -> Result<OcrResult, OcrError> {
        self.detect_base(
            img_src,
            padding,
            max_side_len,
            box_score_thresh,
            box_thresh,
            un_clip_ratio,
            do_angle,
            most_angle,
            false,
            0.0,
        )
    }

    /// 支持角度回滚的检测图片
    /// 在 do_angle 为 true 时生效，如果图片经过了角度纠正，但识别效果过差，则取消角度纠正
    ///
    /// # Arguments
    ///
    /// - `&self` (`undefined`) - Describe this parameter.
    /// - `img_src` (`&image`) - 图片
    /// - `padding` (`u32`) - 变换图片时添加的边框的宽度（提高检测效果）
    /// - `max_side_len` (`u32`) - 变换图片后图片宽和高保留的最大边长（超出该尺寸的图片将缩小）
    /// - `box_score_thresh` (`f32`) - 检测存在文本的区域的分值阈值
    /// - `do_angle` (`bool`) - 是否进行角度检测
    /// - `angle_rollback_threshold` (`f32`) - 角度回滚的阈值，如果识别到的文字得分低于该值（或等于 NaN），则取消角度回滚
    /// ```
    pub fn detect_angle_rollback(
        &mut self,
        img_src: &image::RgbImage,
        padding: u32,
        max_side_len: u32,
        box_score_thresh: f32,
        box_thresh: f32,
        un_clip_ratio: f32,
        do_angle: bool,
        most_angle: bool,
        angle_rollback_threshold: f32,
    ) -> Result<OcrResult, OcrError> {
        self.detect_base(
            img_src,
            padding,
            max_side_len,
            box_score_thresh,
            box_thresh,
            un_clip_ratio,
            do_angle,
            most_angle,
            true,
            angle_rollback_threshold,
        )
    }

    pub fn detect_from_path(
        &mut self,
        img_path: &str,
        padding: u32,
        max_side_len: u32,
        box_score_thresh: f32,
        box_thresh: f32,
        un_clip_ratio: f32,
        do_angle: bool,
        most_angle: bool,
    ) -> Result<OcrResult, OcrError> {
        let img_src = image::open(img_path)?.to_rgb8();

        self.detect(
            &img_src,
            padding,
            max_side_len,
            box_score_thresh,
            box_thresh,
            un_clip_ratio,
            do_angle,
            most_angle,
        )
    }

    fn detect_once(
        &mut self,
        img_src: &image::RgbImage,
        scale: &ScaleParam,
        opts: &DetectOptions,
    ) -> Result<OcrResult, OcrError> {
        // Stage timings, gated on OCR_PROFILE=1. Guessing at the split cost two
        // wrong optimisation targets; this makes the attribution exact.
        let prof = std::env::var_os("OCR_PROFILE").is_some();
        let t = std::time::Instant::now();
        macro_rules! mark {
            ($label:expr) => {
                if prof {
                    eprintln!("OCR_PROFILE {:>22}: {:>8.1}ms", $label, t.elapsed().as_secs_f64() * 1000.0);
                }
            };
        }
        let text_boxes = self.db_net.get_text_boxes(
            img_src,
            scale,
            opts.box_score_thresh,
            opts.box_thresh,
            opts.un_clip_ratio,
            opts.post,
        )?;

        mark!("detect(+post)");
        let part_images = OcrUtils::get_part_images(img_src, &text_boxes);
        mark!("crop_extract");

        let angles = self
            .angle_net
            .get_angles(&part_images, opts.do_angle, opts.most_angle)?;
        mark!("angle");

        let mut rotated_images: Vec<image::RgbImage> = Vec::with_capacity(part_images.len());

        // 角度纠正回滚
        let mut angle_rollback_records =
            HashMap::<usize, ImageBuffer<image::Rgb<u8>, Vec<u8>>>::new();

        for (index, (angle, mut part_image)) in
            angles.iter().zip(part_images.into_iter()).enumerate()
        {
            if angle.index == 1 {
                if opts.angle_rollback {
                    // 保留原始副本
                    angle_rollback_records.insert(index, part_image.clone());
                }

                OcrUtils::mat_rotate_clock_wise_180(&mut part_image);
            }
            rotated_images.push(part_image);
        }

        mark!("rotate");
        let text_lines = self.crnn_net.get_text_lines_batched(
            &rotated_images,
            &angle_rollback_records,
            opts.angle_rollback_threshold,
            opts.rec,
        )?;

        mark!("recognise");
        if prof {
            eprintln!("OCR_PROFILE {:>22}: {}", "boxes", text_boxes.len());
        }
        let mut text_blocks = Vec::with_capacity(text_lines.len());
        for i in 0..text_lines.len() {
            text_blocks.push(TextBlock {
                box_points: text_boxes[i]
                    .points
                    .iter()
                    .map(|p| Point {
                        x: ((p.x as f32) - opts.padding as f32) as u32,
                        y: ((p.y as f32) - opts.padding as f32) as u32,
                    })
                    .collect(),
                box_score: text_boxes[i].score,
                angle_index: angles[i].index,
                angle_score: angles[i].score,
                text: text_lines[i].text.clone(),
                text_score: text_lines[i].text_score,
            });
        }

        Ok(OcrResult { text_blocks })
    }
}
