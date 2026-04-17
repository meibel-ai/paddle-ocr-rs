use crate::{
    ocr_error::OcrError,
    ocr_result::{Point, TextBox},
};
use image::imageops;
use imageproc::geometric_transformations::{Interpolation, Projection};
use ndarray::{Array, Array4};

pub struct OcrUtils;

impl OcrUtils {
    pub fn substract_mean_normalize(
        img_src: &image::RgbImage,
        mean_vals: &[f32],
        norm_vals: &[f32],
    ) -> Array4<f32> {
        let cols = img_src.width();
        let rows = img_src.height();
        let channels = 3;

        let mut input_tensor = Array::zeros((1, channels as usize, rows as usize, cols as usize));

        // 获取图像数据
        unsafe {
            for r in 0..rows {
                for c in 0..cols {
                    for ch in 0..channels {
                        let idx = (r * cols * channels + c * channels + ch) as usize;
                        let value = img_src.get_unchecked(idx).to_owned();
                        let data = value as f32 * norm_vals[ch as usize]
                            - mean_vals[ch as usize] * norm_vals[ch as usize];
                        input_tensor[[0, ch as usize, r as usize, c as usize]] = data;
                    }
                }
            }
        }

        input_tensor
    }

    pub fn make_padding(
        img_src: &image::RgbImage,
        padding: u32,
    ) -> Result<image::RgbImage, OcrError> {
        if padding == 0 {
            return Ok(img_src.clone());
        }

        let width = img_src.width();
        let height = img_src.height();

        let mut padding_src = image::RgbImage::new(width + 2 * padding, height + 2 * padding);
        imageproc::drawing::draw_filled_rect_mut(
            &mut padding_src,
            imageproc::rect::Rect::at(0, 0).of_size(width + 2 * padding, height + 2 * padding),
            image::Rgb([255, 255, 255]),
        );

        image::imageops::replace(&mut padding_src, img_src, padding as i64, padding as i64);

        Ok(padding_src)
    }

    pub fn get_part_images(
        img_src: &image::RgbImage,
        text_boxes: &[TextBox],
    ) -> Vec<image::RgbImage> {
        text_boxes
            .iter()
            .map(|text_box| Self::get_rotate_crop_image(img_src, &text_box.points))
            .collect()
    }

    pub fn get_rotate_crop_image(
        img_src: &image::RgbImage,
        box_points: &[Point],
    ) -> image::RgbImage {
        let mut points = box_points.to_vec();

        // 计算边界框
        let (min_x, min_y, max_x, max_y) = points.iter().fold(
            (u32::MAX, u32::MAX, 0u32, 0u32),
            |(min_x, min_y, max_x, max_y), point| {
                (
                    min_x.min(point.x),
                    min_y.min(point.y),
                    max_x.max(point.x),
                    max_y.max(point.y),
                )
            },
        );

        // 裁剪图像
        let img_crop =
            imageops::crop_imm(img_src, min_x, min_y, max_x - min_x, max_y - min_y).to_image();

        for point in &mut points {
            point.x -= min_x;
            point.y -= min_y;
        }

        // Use max of opposing edges to handle skewed text (matches Python cv2 implementation)
        let dist01 = ((points[0].x as i32 - points[1].x as i32).pow(2) as f32
            + (points[0].y as i32 - points[1].y as i32).pow(2) as f32)
            .sqrt();
        let dist23 = ((points[2].x as i32 - points[3].x as i32).pow(2) as f32
            + (points[2].y as i32 - points[3].y as i32).pow(2) as f32)
            .sqrt();
        let dist03 = ((points[0].x as i32 - points[3].x as i32).pow(2) as f32
            + (points[0].y as i32 - points[3].y as i32).pow(2) as f32)
            .sqrt();
        let dist12 = ((points[1].x as i32 - points[2].x as i32).pow(2) as f32
            + (points[1].y as i32 - points[2].y as i32).pow(2) as f32)
            .sqrt();
        let img_crop_width = dist01.max(dist23) as u32;
        let img_crop_height = dist03.max(dist12) as u32;

        // Border replicate: extend the crop by 2px on each edge using edge-pixel
        // replication. This prevents white fringing at text boundaries during the
        // perspective warp (matches Python's cv2.BORDER_REPLICATE behavior).
        const BORDER: u32 = 2;
        let crop_w = img_crop.width();
        let crop_h = img_crop.height();
        let padded_w = crop_w + 2 * BORDER;
        let padded_h = crop_h + 2 * BORDER;
        let mut padded = image::RgbImage::new(padded_w, padded_h);

        // Copy the original crop into the center
        image::imageops::replace(&mut padded, &img_crop, BORDER as i64, BORDER as i64);

        // Replicate edges (top/bottom rows, left/right columns, then corners)
        for x in 0..crop_w {
            let top_px = *img_crop.get_pixel(x, 0);
            let bot_px = *img_crop.get_pixel(x, crop_h.saturating_sub(1));
            for b in 0..BORDER {
                padded.put_pixel(x + BORDER, b, top_px);
                padded.put_pixel(x + BORDER, padded_h - 1 - b, bot_px);
            }
        }
        for y in 0..crop_h {
            let left_px = *img_crop.get_pixel(0, y);
            let right_px = *img_crop.get_pixel(crop_w.saturating_sub(1), y);
            for b in 0..BORDER {
                padded.put_pixel(b, y + BORDER, left_px);
                padded.put_pixel(padded_w - 1 - b, y + BORDER, right_px);
            }
        }
        // Corners: replicate corner pixels
        let tl = *img_crop.get_pixel(0, 0);
        let tr = *img_crop.get_pixel(crop_w.saturating_sub(1), 0);
        let bl = *img_crop.get_pixel(0, crop_h.saturating_sub(1));
        let br = *img_crop.get_pixel(crop_w.saturating_sub(1), crop_h.saturating_sub(1));
        for by in 0..BORDER {
            for bx in 0..BORDER {
                padded.put_pixel(bx, by, tl);
                padded.put_pixel(padded_w - 1 - bx, by, tr);
                padded.put_pixel(bx, padded_h - 1 - by, bl);
                padded.put_pixel(padded_w - 1 - bx, padded_h - 1 - by, br);
            }
        }

        // Adjust source control points by +BORDER to account for the padded image
        let src_points = [
            (points[0].x as f32 + BORDER as f32, points[0].y as f32 + BORDER as f32),
            (points[1].x as f32 + BORDER as f32, points[1].y as f32 + BORDER as f32),
            (points[2].x as f32 + BORDER as f32, points[2].y as f32 + BORDER as f32),
            (points[3].x as f32 + BORDER as f32, points[3].y as f32 + BORDER as f32),
        ];

        let dst_points = [
            (0.0, 0.0),
            (img_crop_width as f32, 0.0),
            (img_crop_width as f32, img_crop_height as f32),
            (0.0, img_crop_height as f32),
        ];

        let projection = Projection::from_control_points(src_points, dst_points)
            .expect("Failed to create projection transformation");

        let mut part_img = image::RgbImage::new(img_crop_width, img_crop_height);
        imageproc::geometric_transformations::warp_into(
            &padded,
            &projection,
            Interpolation::Bilinear,
            image::Rgb([255, 255, 255]),
            &mut part_img,
        );

        // 根据需要旋转图像
        if part_img.height() >= part_img.width() * 3 / 2 {
            let mut rotated = image::RgbImage::new(part_img.height(), part_img.width());

            for (x, y, pixel) in part_img.enumerate_pixels() {
                rotated.put_pixel(y, part_img.width() - 1 - x, *pixel);
            }

            rotated
        } else {
            part_img
        }
    }

    pub fn mat_rotate_clock_wise_180(src: &mut image::RgbImage) {
        imageops::rotate180_in_place(src);
    }

    pub fn calculate_mean_with_mask(
        img: &image::ImageBuffer<image::Luma<f32>, Vec<f32>>,
        mask: &image::ImageBuffer<image::Luma<u8>, Vec<u8>>,
    ) -> f32 {
        let mut sum: f32 = 0.0;
        let mut mask_count = 0;

        assert_eq!(img.width(), mask.width());
        assert_eq!(img.height(), mask.height());

        for y in 0..img.height() {
            for x in 0..img.width() {
                let mask_value = mask.get_pixel(x, y)[0];
                if mask_value > 0 {
                    let pixel = img.get_pixel(x, y);
                    sum += pixel[0];
                    mask_count += 1;
                }
            }
        }

        if mask_count == 0 {
            return 0.0;
        }

        sum / mask_count as f32
    }
}
