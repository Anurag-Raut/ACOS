//! Compare ACOS text metrics against the real Android 34 emulator UI dump for
//! the hello-views fixture.
//!
//! Reference bounds come from
//! `fixtures/reference/hello-views/art-api34-x86_64/ui_initial.xml`
//! (1080x2400 @ 420dpi). The TextView is wrap_content, so its bounds width is
//! the advance width of the string and its height is `descent - ascent` plus
//! font padding. We render the same strings with the real Roboto font and
//! report the pixel deltas.

use std::path::PathBuf;

use acos_render::RenderContext;

fn reference_xml() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/reference/hello-views/art-api34-x86_64/ui_initial.xml");
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

/// Find bounds for a node with the given resource-id in the single-line UI dump.
fn bounds_for(xml: &str, id: &str) -> Option<[i32; 4]> {
    let marker = format!("resource-id=\"{id}\"");
    let idx = xml.find(&marker)?;
    // `bounds` is near the end of the node; search the next `bounds="` after it.
    let rest = &xml[idx..];
    let b = rest.find("bounds=\"")?;
    let after = &rest[b + 8..];
    let end = after.find('"')?;
    let raw = &after[..end];
    // raw like "[42,317][231,368]": scan all signed integers.
    let mut nums = Vec::new();
    let mut cur = String::new();
    for ch in raw.chars() {
        if ch == '-' || ch.is_ascii_digit() {
            cur.push(ch);
        } else if !cur.is_empty() {
            if let Ok(v) = cur.parse::<i32>() {
                nums.push(v);
            }
            cur.clear();
        }
    }
    if !cur.is_empty() {
        if let Ok(v) = cur.parse::<i32>() {
            nums.push(v);
        }
    }
    if nums.len() >= 4 {
        Some([nums[0], nums[1], nums[2], nums[3]])
    } else {
        None
    }
}

#[test]
fn hello_views_text_metrics_vs_art() {
    let Some(xml_path) = reference_xml() else {
        eprintln!("SKIP: no hello-views reference UI dump at fixtures/reference");
        return;
    };
    let xml = std::fs::read_to_string(&xml_path).unwrap();
    let title = bounds_for(&xml, "dev.acos.fixtures.helloviews:id/title").unwrap();
    let result = bounds_for(&xml, "dev.acos.fixtures.helloviews:id/result").unwrap();
    let ref_title_w = title[2] - title[0];
    let ref_title_h = title[3] - title[1];
    let ref_result_w = result[2] - result[0];
    eprintln!(
        "ART reference title={:?} ({}x{}), result={:?} ({}x{})",
        title, ref_title_w, ref_title_h, result, ref_result_w, ref_title_h
    );

    let mut ctx = RenderContext::new();
    if !ctx.has_fonts() {
        eprintln!("SKIP: no fonts available (set ACOS_FONT_DIR)");
        return;
    }
    // DeviceDefault TextView default size is 14sp at density 2.625.
    let density = 2.625f32;
    let text_size = 14.0 * density;
    let paint = ctx.paint_create();
    ctx.paint_set_text_size(paint, text_size).unwrap();
    let typeface = ctx.typeface_create(None, 400, false).unwrap();
    ctx.paint_set_typeface(paint, typeface).unwrap();

    let title_text: Vec<u16> = "Hello ACOS".encode_utf16().collect();
    let result_text: Vec<u16> = "clicks: 0".encode_utf16().collect();
    let w_title = ctx.paint_measure_text(paint, &title_text).unwrap();
    let w_result = ctx.paint_measure_text(paint, &result_text).unwrap();
    let metrics = ctx.paint_get_font_metrics(paint).unwrap();
    let h_text = metrics.descent - metrics.ascent;

    let dw_title = (w_title - ref_title_w as f32).abs() / ref_title_w as f32 * 100.0;
    let dw_result = (w_result - ref_result_w as f32).abs() / ref_result_w as f32 * 100.0;
    let dh = (h_text - ref_title_h as f32).abs() / ref_title_h as f32 * 100.0;
    eprintln!(
        "ACOS text metrics @14sp: title={:.2}px (ref {}px, delta {:.1}%), result={:.2}px (ref {}px, delta {:.1}%), height={:.2}px (ref {}px, delta {:.1}%)",
        w_title, ref_title_w, dw_title, w_result, ref_result_w, dw_result, h_text, ref_title_h, dh
    );
    eprintln!(
        "font metrics: top={:.2} ascent={:.2} descent={:.2} bottom={:.2} leading={:.2}",
        metrics.top, metrics.ascent, metrics.descent, metrics.bottom, metrics.leading
    );

    // The real emulator image and the host font may differ slightly; assert a
    // bounded fidelity rather than pixel-exact equality.
    assert!(dw_title < 20.0, "title width delta {dw_title:.1}%");
    assert!(dw_result < 20.0, "result width delta {dw_result:.1}%");
    assert!(dh < 25.0, "text height delta {dh:.1}%");
}
