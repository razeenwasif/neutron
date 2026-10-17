//! PDF pages, drawn by Core Graphics into memory for the viewer.
//!
//! Core Graphics, not PDFKit: drawing a page into a bitmap context of its own
//! is safe on any thread, so the viewer renders on a worker and the paint
//! thread only ever uploads finished pixels.

use std::path::Path;

use objc2_core_foundation::{CFRetained, CFURL, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGPDFBox, CGPDFDocument, CGPDFPage,
};

fn open(path: &Path) -> Result<CFRetained<CGPDFDocument>, String> {
    let url = CFURL::from_file_path(path).ok_or("the PDF's path cannot be named to macOS")?;
    let document = CGPDFDocument::with_url(Some(&url)).ok_or("this is not a PDF macOS can read")?;
    if CGPDFDocument::is_encrypted(Some(&document)) && !CGPDFDocument::is_unlocked(Some(&document)) {
        return Err("this PDF is locked with a password".into());
    }
    Ok(document)
}

/// A page's size as it is shown — turned, when it is meant to be — in points.
fn shown_size(page: &CGPDFPage) -> [f32; 2] {
    let rect = CGPDFPage::box_rect(Some(page), CGPDFBox::CropBox);
    let (w, h) = (rect.size.width as f32, rect.size.height as f32);
    if CGPDFPage::rotation_angle(Some(page)).rem_euclid(180) == 90 { [h, w] } else { [w, h] }
}

/// Each page's size, in points. Blocks on the disk: call it from a worker.
pub fn pages(path: &Path) -> Result<Vec<[f32; 2]>, String> {
    let document = open(path)?;
    let count = CGPDFDocument::number_of_pages(Some(&document));
    Ok((1..=count)
        .map(|n| CGPDFDocument::page(Some(&document), n).map_or([612.0, 792.0], |page| shown_size(&page)))
        .collect())
}

/// Page `page` (from zero) drawn `width` pixels wide on white: its width,
/// height and RGBA pixels. Blocks: call it from a worker.
pub fn render(path: &Path, page: usize, width: usize) -> Result<(usize, usize, Vec<u8>), String> {
    let document = open(path)?;
    let page = CGPDFDocument::page(Some(&document), page + 1).ok_or("there is no such page")?;
    let [w, h] = shown_size(&page);
    let width = width.clamp(16, 8192);
    let height = ((h / w) * width as f32).round().max(1.0) as usize;
    let mut pixels = vec![0u8; width * height * 4];
    let space = CGColorSpace::new_device_rgb().ok_or("no colour space")?;
    // SAFETY: `pixels` is `width * height * 4` bytes, rows of `width * 4`,
    // and outlives the context, which is dropped at the end of this function.
    let context = unsafe {
        CGBitmapContextCreate(
            pixels.as_mut_ptr().cast(),
            width,
            height,
            8,
            width * 4,
            Some(&space),
            CGImageAlphaInfo::PremultipliedLast.0,
        )
    }
    .ok_or("no memory to draw the page in")?;
    let all = CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: width as f64, height: height as f64 } };
    CGContext::set_rgb_fill_color(Some(&context), 1.0, 1.0, 1.0, 1.0);
    CGContext::fill_rect(Some(&context), all);
    // Scaled here rather than by the page's own transform, which only ever
    // shrinks a page to fit and never enlarges one: asked for a page at its
    // natural size it does nothing but turn it, which is what it is used for.
    let scale = width as f64 / w as f64;
    CGContext::scale_ctm(Some(&context), scale, scale);
    let natural = CGRect { origin: CGPoint { x: 0.0, y: 0.0 }, size: CGSize { width: w as f64, height: h as f64 } };
    let transform = CGPDFPage::drawing_transform(Some(&page), CGPDFBox::CropBox, natural, 0, true);
    CGContext::concat_ctm(Some(&context), transform);
    CGContext::draw_pdf_page(Some(&context), Some(&page));
    drop(context);
    Ok((width, height, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-page PDF, 100 by 200 points, with a black square in its
    /// lower-left corner — written out with the offsets its table needs.
    fn tiny_pdf() -> Vec<u8> {
        let content = "0 0 0 rg 0 0 50 50 re f";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 200] /Contents 4 0 R >>".to_owned(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
        out
    }

    #[test]
    fn a_page_is_measured_and_drawn_at_the_width_asked_for() {
        let path = std::env::temp_dir().join(format!("neutron-pdf-{}.pdf", std::process::id()));
        std::fs::write(&path, tiny_pdf()).unwrap();
        assert_eq!(pages(&path).unwrap(), vec![[100.0, 200.0]]);
        // Twice its natural size: the page is enlarged, not left small.
        let (w, h, pixels) = render(&path, 0, 200).unwrap();
        assert_eq!((w, h), (200, 400));
        let at = |x: usize, y: usize| &pixels[(y * w + x) * 4..(y * w + x) * 4 + 3];
        assert_eq!(at(10, 390), [0, 0, 0], "the square, at the bottom left");
        assert_eq!(at(150, 10), [255, 255, 255], "white paper at the top right");
        assert_eq!(at(110, 390), [255, 255, 255], "the square ends at half the width");
        let _ = std::fs::remove_file(&path);
    }
}
