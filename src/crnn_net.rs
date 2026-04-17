use ort::session::Session;
use ort::value::Tensor;
use ort::{inputs, session::builder::SessionBuilder};
use std::collections::HashMap;

use crate::{base_net::BaseNet, ocr_error::OcrError, ocr_result::TextLine, ocr_utils::OcrUtils};

const CRNN_DST_HEIGHT: u32 = 48;
const MEAN_VALUES: [f32; 3] = [127.5, 127.5, 127.5];
const NORM_VALUES: [f32; 3] = [1.0 / 127.5, 1.0 / 127.5, 1.0 / 127.5];

#[derive(Debug)]
pub struct CrnnNet {
    session: Option<Session>,
    keys: Vec<String>,
    input_names: Vec<String>,
}

impl BaseNet for CrnnNet {
    fn new() -> Self {
        Self {
            session: None,
            keys: Vec::new(),
            input_names: Vec::new(),
        }
    }

    fn set_input_names(&mut self, input_names: Vec<String>) {
        self.input_names = input_names;
    }

    fn set_session(&mut self, session: Option<Session>) {
        self.session = session;
    }
}

impl CrnnNet {
    pub fn init_model(
        &mut self,
        path: &str,
        num_thread: usize,
        builder_fn: Option<fn(SessionBuilder) -> Result<SessionBuilder, ort::Error>>,
    ) -> Result<(), OcrError> {
        BaseNet::init_model(self, path, num_thread, builder_fn)?;

        self.keys = self.get_keys()?;

        Ok(())
    }

    pub fn init_model_dict_file(
        &mut self,
        path: &str,
        num_thread: usize,
        builder_fn: Option<fn(SessionBuilder) -> Result<SessionBuilder, ort::Error>>,
        dict_file_path: &str,
    ) -> Result<(), OcrError> {
        BaseNet::init_model(self, path, num_thread, builder_fn)?;

        self.read_keys_from_file(dict_file_path)?;

        Ok(())
    }

    pub fn init_model_from_memory(
        &mut self,
        model_bytes: &[u8],
        num_thread: usize,
        builder_fn: Option<fn(SessionBuilder) -> Result<SessionBuilder, ort::Error>>,
    ) -> Result<(), OcrError> {
        BaseNet::init_model_from_memory(self, model_bytes, num_thread, builder_fn)?;

        self.keys = self.get_keys()?;

        Ok(())
    }

    fn get_keys(&mut self) -> Result<Vec<String>, OcrError> {
        // 简单处理下报错，模型正确的话并无概率出错
        let model_charater_list = self
            .session
            .as_ref()
            .expect("crnn_net session not initialized")
            .metadata()
            .expect("crnn_net metadata not initialized")
            .custom("character")
            .expect("crnn_net character not initialized");

        // 大概估一个数即可
        let mut keys = Vec::with_capacity((model_charater_list.len() as f32 / 3.9) as usize);

        keys.push("#".to_string());

        keys.extend(model_charater_list.split('\n').map(|s: &str| s.to_string()));

        keys.push(" ".to_string());

        Ok(keys)
    }

    fn read_keys_from_file(&mut self, path: &str) -> Result<(), OcrError> {
        let content = std::fs::read_to_string(path)?;
        let mut keys = Vec::new();

        // Index 0 = CTC blank token (must match get_keys() which prepends "#")
        keys.push("#".to_string());
        // Filter empty lines (trailing newline in dict.txt creates one)
        keys.extend(
            content
                .split('\n')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string()),
        );
        // Trailing space token (must match get_keys() which appends " ")
        keys.push(" ".to_string());

        self.keys = keys;
        Ok(())
    }

    pub fn get_text_lines(
        &mut self,
        part_imgs: &[image::RgbImage],
        angle_rollback_records: &HashMap<usize, image::RgbImage>,
        angle_rollback_threshold: f32,
    ) -> Result<Vec<TextLine>, OcrError> {
        let mut text_lines = Vec::new();

        // Compute max width/height ratio across all images in the batch.
        // This matches Python PaddleOCR's batch processing where all images
        // are padded to the same width (48 * max_wh_ratio).
        // Minimum is 320/48 ≈ 6.667 (Python's default rec_img_shape width).
        let base_wh_ratio = 320.0 / CRNN_DST_HEIGHT as f32;
        let max_wh_ratio = part_imgs
            .iter()
            .map(|img| img.width() as f32 / img.height().max(1) as f32)
            .fold(base_wh_ratio, f32::max);

        for (index, img) in part_imgs.iter().enumerate() {
            let mut text_line = self.get_text_line_with_wh_ratio(img, max_wh_ratio)?;

            if (text_line.text_score.is_nan() || text_line.text_score < angle_rollback_threshold)
                && let Some(angle_rollback_record) = angle_rollback_records.get(&index)
            {
                text_line = self.get_text_line_with_wh_ratio(angle_rollback_record, max_wh_ratio)?;
            }

            text_lines.push(text_line);
        }

        Ok(text_lines)
    }

    /// Recognize a single text line image with an optional max width/height ratio
    /// for padding. When `max_wh_ratio > 0`, the normalized tensor is zero-padded
    /// on the right to `(48 * max_wh_ratio)` pixels. This matches Python PaddleOCR's
    /// `resize_norm_img` which pads to a fixed batch width.
    fn get_text_line_with_wh_ratio(
        &mut self,
        img_src: &image::RgbImage,
        max_wh_ratio: f32,
    ) -> Result<TextLine, OcrError> {
        let Some(session) = &mut self.session else {
            return Err(OcrError::SessionNotInitialized);
        };

        let scale = CRNN_DST_HEIGHT as f32 / img_src.height() as f32;
        let resized_w = (img_src.width() as f32 * scale).ceil() as u32;

        let src_resize = image::imageops::resize(
            img_src,
            resized_w,
            CRNN_DST_HEIGHT,
            image::imageops::FilterType::Triangle,
        );

        let input_tensors =
            OcrUtils::substract_mean_normalize(&src_resize, &MEAN_VALUES, &NORM_VALUES);

        // Zero-pad to the target width if max_wh_ratio is specified.
        // Python PaddleOCR pads recognition inputs to (48 * max_wh_ratio) with zeros.
        // Zero in normalized space = (0/127.5 - 1.0) = -1.0, but Python uses actual
        // 0.0 in its padded tensor (the padding is applied AFTER normalization).
        let input_tensors = if max_wh_ratio > 0.0 {
            let target_w = (CRNN_DST_HEIGHT as f32 * max_wh_ratio) as u32;
            let target_w = target_w.max(resized_w); // never shrink
            if target_w > resized_w {
                let shape = input_tensors.shape();
                let c = shape[1];
                let h = shape[2];
                let mut padded = ndarray::Array4::<f32>::zeros((1, c, h, target_w as usize));
                padded
                    .slice_mut(ndarray::s![.., .., .., ..resized_w as usize])
                    .assign(&input_tensors);
                padded
            } else {
                input_tensors
            }
        } else {
            input_tensors
        };

        let input_tensors = Tensor::from_array(input_tensors)?;

        let outputs = session.run(inputs![self.input_names[0].clone() => input_tensors])?;

        let (_, red_data) = outputs.iter().next().unwrap();

        let (shape, src_data) = red_data.try_extract_tensor::<f32>()?;
        let dimensions = shape;
        let height = dimensions[1] as usize;
        let width = dimensions[2] as usize;
        let src_data: Vec<f32> = src_data.to_vec();

        Self::score_to_text_line(&src_data, height, width, &self.keys)
    }

    fn score_to_text_line(
        output_data: &[f32],
        height: usize,
        width: usize,
        keys: &[String],
    ) -> Result<TextLine, OcrError> {
        let mut text_line = TextLine::default();
        let mut last_index = 0;
        let mut text_score_sum = 0.0;
        let mut text_score_count = 0;

        for i in 0..height {
            let start = i * width;
            let stop = (i + 1) * width;
            let slice = &output_data[start..stop.min(output_data.len())];

            let (max_index, max_value) =
                slice
                    .iter()
                    .enumerate()
                    .fold((0, f32::MIN), |(max_idx, max_val), (idx, &val)| {
                        if val > max_val {
                            (idx, val)
                        } else {
                            (max_idx, max_val)
                        }
                    });

            if max_index > 0 && max_index < keys.len() && !(i > 0 && max_index == last_index) {
                text_line.text.push_str(&keys[max_index]);
                text_score_sum += max_value;
                text_score_count += 1;
            }
            last_index = max_index;
        }

        text_line.text_score = if text_score_count > 0 {
            text_score_sum / text_score_count as f32
        } else {
            0.0
        };
        Ok(text_line)
    }
}
