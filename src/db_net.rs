use crate::{
    base_net::BaseNet,
    ocr_error::OcrError,
    ocr_result::{self, TextBox},
    ocr_utils::OcrUtils,
    scale_param::ScaleParam,
};
use geo_clipper::{Clipper, EndType, JoinType};
use geo_types::{Coord, LineString, Polygon};
use ort::{inputs, session::SessionOutputs};
use ort::{session::Session, value::Tensor};
use std::cmp::Ordering;

const MEAN_VALUES: [f32; 3] = [
    0.485_f32 * 255_f32,
    0.456_f32 * 255_f32,
    0.406_f32 * 255_f32,
];
const NORM_VALUES: [f32; 3] = [
    1.0_f32 / 0.229_f32 / 255.0_f32,
    1.0_f32 / 0.224_f32 / 255.0_f32,
    1.0_f32 / 0.225_f32 / 255.0_f32,
];

#[derive(Debug)]
pub struct DbNet {
    session: Option<Session>,
    input_names: Vec<String>,
}

impl BaseNet for DbNet {
    fn new() -> Self {
        Self {
            session: None,
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

/// Post-processing knobs that PaddleOCR exposes and this crate previously
/// hard-coded.
#[derive(Debug, Clone, Copy)]
pub struct PostProcessOptions {
    /// Dilate the binarized map by one pixel before finding contours.
    ///
    /// PaddleOCR's `use_dilation` defaults to **false**, and this crate used to
    /// do it unconditionally. Every box therefore came out fatter than
    /// reference in all four directions — which compounds with `un_clip_ratio`
    /// and, on dense tables, lets a box bleed into the neighbouring cell.
    pub use_dilation: bool,
    /// Cap on contours considered, highest-scoring first.
    ///
    /// PaddleOCR caps this (1000 by default, 3000 in PP-OCRv6's config); this
    /// crate used to consider every contour found. That is a latency cliff on a
    /// noisy scan with no accuracy upside, and it got sharper in v6, whose
    /// lower binarization threshold leaves more contours standing.
    pub max_candidates: usize,
}

impl Default for PostProcessOptions {
    fn default() -> Self {
        // Reference PaddleOCR, with PP-OCRv6's candidate cap.
        Self {
            use_dilation: false,
            max_candidates: 3000,
        }
    }
}

impl PostProcessOptions {
    /// This crate's historical behaviour: always dilate, never cap.
    pub fn legacy() -> Self {
        Self {
            use_dilation: true,
            max_candidates: usize::MAX,
        }
    }
}

impl DbNet {
    pub fn get_text_boxes(
        &mut self,
        img_src: &image::RgbImage,
        scale: &ScaleParam,
        box_score_thresh: f32,
        box_thresh: f32,
        un_clip_ratio: f32,
        opts: PostProcessOptions,
    ) -> Result<Vec<TextBox>, OcrError> {
        let Some(session) = &mut self.session else {
            return Err(OcrError::SessionNotInitialized);
        };

        let src_resize = image::imageops::resize(
            img_src,
            scale.dst_width,
            scale.dst_height,
            image::imageops::FilterType::Triangle,
        );

        let input_tensors =
            OcrUtils::substract_mean_normalize(&src_resize, &MEAN_VALUES, &NORM_VALUES);

        let tensor = Tensor::from_array(input_tensors)?;

        let prof = std::env::var_os("OCR_PROFILE").is_some();
        let t0 = std::time::Instant::now();
        let outputs = session.run(inputs![self.input_names[0].clone() => tensor])?;
        if prof {
            eprintln!("OCR_PROFILE {:>22}: {:>8.1}ms", "det_infer", t0.elapsed().as_secs_f64() * 1000.0);
        }
        let t1 = std::time::Instant::now();

        let text_boxes = Self::get_text_boxes_core(
            &outputs,
            src_resize.height(),
            src_resize.width(),
            &ScaleParam::new(
                scale.src_width,
                scale.src_height,
                scale.dst_width,
                scale.dst_height,
                scale.scale_width,
                scale.scale_height,
            ),
            box_score_thresh,
            box_thresh,
            un_clip_ratio,
            opts,
        )?;

        if prof {
            eprintln!("OCR_PROFILE {:>22}: {:>8.1}ms", "db_postprocess", t1.elapsed().as_secs_f64() * 1000.0);
        }
        Ok(text_boxes)
    }

    fn get_text_boxes_core(
        output_tensor: &SessionOutputs,
        rows: u32,
        cols: u32,
        s: &ScaleParam,
        box_score_thresh: f32,
        box_thresh: f32,
        un_clip_ratio: f32,
        opts: PostProcessOptions,
    ) -> Result<Vec<TextBox>, OcrError> {
        let max_side_thresh = 3.0;
        let mut rs_boxes = Vec::new();

        let (_, red_data) = output_tensor.iter().next().unwrap();

        let pred_data: Vec<f32> = red_data.try_extract_tensor::<f32>()?.1.to_vec();

        let cbuf_data: Vec<u8> = pred_data
            .iter()
            .map(|pixel| (pixel * 255.0) as u8)
            .collect();

        let pred_img: image::ImageBuffer<image::Luma<f32>, Vec<f32>> =
            image::ImageBuffer::from_vec(cols, rows, pred_data).unwrap();

        let cbuf_img = image::GrayImage::from_vec(cols, rows, cbuf_data).unwrap();

        let threshold_img = imageproc::contrast::threshold(
            &cbuf_img,
            (box_thresh * 255.0) as u8,
            imageproc::contrast::ThresholdType::Binary,
        );

        let contour_src = if opts.use_dilation {
            std::borrow::Cow::Owned(imageproc::morphology::dilate(
                &threshold_img,
                imageproc::distance_transform::Norm::LInf,
                1,
            ))
        } else {
            std::borrow::Cow::Borrowed(&threshold_img)
        };

        let mut img_contours: Vec<imageproc::contours::Contour<i32>> =
            imageproc::contours::find_contours(contour_src.as_ref());

        // Cap the candidate count the way PaddleOCR does. Biggest-first is the
        // usable proxy for its score ordering: the real score needs the mask
        // fill and ROI mean computed below, so ordering by it here would mean
        // paying the full per-contour cost for every contour — which is the
        // cost this cap exists to avoid. Area correlates well enough, and text
        // regions are not the small ones.
        if img_contours.len() > opts.max_candidates {
            img_contours.sort_unstable_by_key(|c| std::cmp::Reverse(c.points.len()));
            img_contours.truncate(opts.max_candidates);
        }

        for contour in img_contours {
            if contour.points.len() <= 2 {
                continue;
            }

            let mut max_side = 0.0;
            let min_box = Self::get_mini_box(&contour.points, &mut max_side)?;
            if max_side < max_side_thresh {
                continue;
            }

            let score = Self::get_score(&contour, &pred_img)?;
            if score < box_score_thresh {
                continue;
            }

            let clip_box = Self::unclip(&min_box, un_clip_ratio)?;
            if clip_box.is_empty() {
                continue;
            }

            let mut clip_contour = Vec::new();
            for point in &clip_box {
                clip_contour.push(*point);
            }

            let mut max_side_clip = 0.0;
            let clip_min_box = Self::get_mini_box(&clip_contour, &mut max_side_clip)?;
            if max_side_clip < max_side_thresh + 2.0 {
                continue;
            }

            let mut final_points = Vec::new();
            for item in clip_min_box {
                let x = (item.x / s.scale_width) as u32;
                let ptx = x.min(s.src_width);

                let y = (item.y / s.scale_height) as u32;
                let pty = y.min(s.src_height);

                final_points.push(ocr_result::Point { x: ptx, y: pty });
            }

            let text_box = TextBox {
                score,
                points: final_points,
            };

            rs_boxes.push(text_box);
        }

        Ok(rs_boxes)
    }

    fn get_mini_box(
        contour_points: &[imageproc::point::Point<i32>],
        min_edge_size: &mut f32,
    ) -> Result<Vec<imageproc::point::Point<f32>>, OcrError> {
        let rect = imageproc::geometry::min_area_rect(contour_points);

        let mut rect_points: Vec<imageproc::point::Point<f32>> = rect
            .iter()
            .map(|p| imageproc::point::Point::new(p.x as f32, p.y as f32))
            .collect();

        let width = ((rect_points[0].x - rect_points[1].x).powi(2)
            + (rect_points[0].y - rect_points[1].y).powi(2))
        .sqrt();
        let height = ((rect_points[1].x - rect_points[2].x).powi(2)
            + (rect_points[1].y - rect_points[2].y).powi(2))
        .sqrt();

        *min_edge_size = width.min(height);

        rect_points.sort_by(|a, b| {
            if a.x > b.x {
                return Ordering::Greater;
            }
            if a.x == b.x {
                return Ordering::Equal;
            }
            Ordering::Less
        });

        let mut box_points = Vec::new();
        let index_1;
        let index_4;
        if rect_points[1].y > rect_points[0].y {
            index_1 = 0;
            index_4 = 1;
        } else {
            index_1 = 1;
            index_4 = 0;
        }

        let index_2;
        let index_3;
        if rect_points[3].y > rect_points[2].y {
            index_2 = 2;
            index_3 = 3;
        } else {
            index_2 = 3;
            index_3 = 2;
        }

        box_points.push(rect_points[index_1]);
        box_points.push(rect_points[index_2]);
        box_points.push(rect_points[index_3]);
        box_points.push(rect_points[index_4]);

        Ok(box_points)
    }

    fn get_score(
        contour: &imageproc::contours::Contour<i32>,
        f_map_mat: &image::ImageBuffer<image::Luma<f32>, Vec<f32>>,
    ) -> Result<f32, OcrError> {
        // 初始化边界值
        let mut xmin = i32::MAX;
        let mut xmax = i32::MIN;
        let mut ymin = i32::MAX;
        let mut ymax = i32::MIN;

        // 找到轮廓的边界框
        for point in contour.points.iter() {
            let x = point.x;
            let y = point.y;

            if x < xmin {
                xmin = x;
            }
            if x > xmax {
                xmax = x;
            }
            if y < ymin {
                ymin = y;
            }
            if y > ymax {
                ymax = y;
            }
        }

        let width = f_map_mat.width() as i32;
        let height = f_map_mat.height() as i32;

        xmin = xmin.max(0).min(width - 1);
        xmax = xmax.max(0).min(width - 1);
        ymin = ymin.max(0).min(height - 1);
        ymax = ymax.max(0).min(height - 1);

        let roi_width = xmax - xmin + 1;
        let roi_height = ymax - ymin + 1;

        if roi_width <= 0 || roi_height <= 0 {
            return Ok(0.0);
        }

        let mut mask = image::GrayImage::new(roi_width as u32, roi_height as u32);

        let mut pts = Vec::<imageproc::point::Point<i32>>::new();
        for point in contour.points.iter() {
            pts.push(imageproc::point::Point::new(point.x - xmin, point.y - ymin));
        }

        imageproc::drawing::draw_polygon_mut(&mut mask, pts.as_slice(), image::Luma([255]));

        let cropped_img = image::imageops::crop_imm(
            f_map_mat,
            xmin as u32,
            ymin as u32,
            roi_width as u32,
            roi_height as u32,
        )
        .to_image();

        let mean = OcrUtils::calculate_mean_with_mask(&cropped_img, &mask);

        Ok(mean)
    }

    fn unclip(
        box_points: &[imageproc::point::Point<f32>],
        unclip_ratio: f32,
    ) -> Result<Vec<imageproc::point::Point<i32>>, OcrError> {
        let points_arr = box_points.to_vec();

        let clip_rect_width = ((points_arr[0].x - points_arr[1].x).powi(2)
            + (points_arr[0].y - points_arr[1].y).powi(2))
        .sqrt();
        let clip_rect_height = ((points_arr[1].x - points_arr[2].x).powi(2)
            + (points_arr[1].y - points_arr[2].y).powi(2))
        .sqrt();

        if clip_rect_height < 1.001 && clip_rect_width < 1.001 {
            return Ok(Vec::new());
        }

        let mut the_cliper_pts = Vec::new();
        for pt in box_points {
            let a1 = Coord {
                x: pt.x as f64,
                y: pt.y as f64,
            };
            the_cliper_pts.push(a1);
        }

        let area = Self::signed_polygon_area(box_points).abs();
        let length = Self::length_of_points(box_points);
        let distance = area * unclip_ratio / length as f32;

        let co = Polygon::new(LineString::new(the_cliper_pts), vec![]);
        let solution = co
            .offset(
                distance as f64,
                JoinType::Round(2.0),
                EndType::ClosedPolygon,
                1.0,
            )
            .0;

        if solution.is_empty() {
            return Ok(Vec::new());
        }

        let mut ret_pts = Vec::new();
        for ip in solution.first().unwrap().exterior().points() {
            ret_pts.push(imageproc::point::Point::new(ip.x() as i32, ip.y() as i32));
        }

        Ok(ret_pts)
    }

    fn signed_polygon_area(points: &[imageproc::point::Point<f32>]) -> f32 {
        let num_points = points.len();
        let mut pts = Vec::with_capacity(num_points + 1);
        pts.extend_from_slice(points);
        pts.push(points[0]);

        let mut area = 0.0;
        for i in 0..num_points {
            area += (pts[i + 1].x - pts[i].x) * (pts[i + 1].y + pts[i].y) / 2.0;
        }

        area
    }

    fn length_of_points(box_points: &[imageproc::point::Point<f32>]) -> f64 {
        if box_points.is_empty() {
            return 0.0;
        }

        let mut length = 0.0;
        let pt = box_points[0];
        let mut x0 = pt.x as f64;
        let mut y0 = pt.y as f64;

        let mut box_with_first = Vec::from(box_points);
        box_with_first.push(pt);

        (1..box_with_first.len()).for_each(|idx| {
            let pts = box_with_first[idx];
            let x1 = pts.x as f64;
            let y1 = pts.y as f64;
            let dx = x1 - x0;
            let dy = y1 - y0;

            length += (dx * dx + dy * dy).sqrt();

            x0 = x1;
            y0 = y1;
        });

        length
    }
}
