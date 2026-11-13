//! Geometry regressions at the shared SVG seam, including shaped text bounds.
use crate::{design::Design, sheet::*};
use resvg::usvg;
use serde_json::json;

fn sample(kind: ChartKind) -> Sheet {
    let mut sheet = apply(
        &Sheet::from_design("Layout", &Design::default()),
        &serde_json::from_value::<Vec<Op>>(json!([
            {"op":"set_cells","tab":"Sheet1","at":"A1","values":[
                ["Month","Revenue","Costs","Profit"],
                ["Jan","15000","10000","5000"],
                ["Feb","25000","15000","10000"],
                ["Mar","45000","20000","25000"]
            ]},
            {"op":"format","tab":"Sheet1","range":"B2:D4","patch":{"num":"$#,##0"}},
            {"op":"add_chart","tab":"Sheet1","data":"A1:D4"}
        ]))
        .unwrap(),
    )
    .unwrap();
    sheet.tabs[0].charts[0].kind = kind;
    sheet
}

fn render(sheet: &Sheet) -> String {
    chart_svg::svg(
        sheet,
        &Workbook::load(sheet).unwrap(),
        &sheet.tabs[0],
        &sheet.tabs[0].charts[0],
    )
}

fn attribute(element: &str, name: &str) -> Option<String> {
    element
        .split_once(&format!("{name}=\""))
        .map(|(_, rest)| rest.split('"').next().unwrap().to_string())
}
fn number(element: &str, name: &str) -> f64 {
    attribute(element, name).unwrap().parse().unwrap()
}
fn viewport(svg: &str) -> &str {
    svg.split("<svg ")
        .nth(2)
        .unwrap()
        .split('>')
        .next()
        .unwrap()
}
fn plot(svg: &str) -> [f64; 4] {
    let element = viewport(svg);
    [
        number(element, "x"),
        number(element, "y"),
        number(element, "width"),
        number(element, "height"),
    ]
}

fn text_boxes(sheet: &Sheet, svg: &str) -> Vec<(String, [f64; 4])> {
    fn collect(group: &usvg::Group, boxes: &mut Vec<(String, [f64; 4])>) {
        for node in group.children() {
            match node {
                usvg::Node::Group(group) => collect(group, boxes),
                usvg::Node::Text(text) => {
                    let bounds = text.abs_bounding_box();
                    boxes.push((
                        text.chunks()
                            .iter()
                            .map(usvg::TextChunk::text)
                            .collect::<String>(),
                        [
                            f64::from(bounds.left()),
                            f64::from(bounds.top()),
                            f64::from(bounds.right()),
                            f64::from(bounds.bottom()),
                        ],
                    ));
                }
                _ => {}
            }
        }
    }
    let tree = usvg::Tree::from_str(
        svg,
        &usvg::Options {
            fontdb: crate::deck::text::deck_fonts(&crate::sheet::render::theme(sheet), None),
            ..Default::default()
        },
    )
    .unwrap();
    let mut boxes = Vec::new();
    collect(tree.root(), &mut boxes);
    boxes
}

#[test]
fn sheet_chart_right_legend_width_is_measured_and_long_names_are_ellipsized() {
    let mut sheet = sample(ChartKind::Column);
    sheet.tabs[0].cells.get_mut("B1").unwrap().input =
        "Revenue generated from long-term subscriptions".into();
    let svg = render(&sheet);
    let bounds = plot(&svg);
    let boxes = text_boxes(&sheet, &svg);
    let legend = boxes
        .iter()
        .find(|(text, _)| text.starts_with("Revenue"))
        .unwrap();
    assert!(legend.0.ends_with('…'));
    assert!(legend.1[0] > bounds[0] + bounds[2]);
    assert!(legend.1[2] - legend.1[0] <= sheet.tabs[0].charts[0].w * 0.35);
    assert!(legend.1[2] < sheet.tabs[0].charts[0].w);
}

#[test]
fn sheet_chart_pie_omits_percentages_that_do_not_fit_and_uses_contrasting_ink() {
    let mut sheet = sample(ChartKind::Pie);
    sheet.theme.colors.insert("bg".into(), "#ffffff".into());
    sheet.theme.colors.insert("fg".into(), "#111111".into());
    for (accent, expected) in [("#111111", "#ffffff"), ("#ffff00", "#111111")] {
        sheet.theme.colors.insert("accent".into(), accent.into());
        let svg = render(&sheet);
        let percentage = svg
            .split("<text ")
            .skip(1)
            .find(|element| element.split("</text>").next().unwrap().ends_with("18%"))
            .unwrap();
        assert_eq!(attribute(percentage, "fill").as_deref(), Some(expected));
    }
    for (row, value) in [(2, "5"), (3, "45"), (4, "50")] {
        sheet.tabs[0]
            .cells
            .get_mut(&format!("B{row}"))
            .unwrap()
            .input = value.into();
    }
    let chart = &mut sheet.tabs[0].charts[0];
    chart.w = 200.0;
    chart.h = 160.0;
    chart.legend = Legend::None;
    let boxes = text_boxes(&sheet, &render(&sheet));
    assert!(!boxes.iter().any(|(text, _)| text == "5%"));
    assert_eq!(
        boxes.iter().filter(|(text, _)| text.contains('%')).count(),
        2
    );
}

#[test]
fn sheet_chart_layouts_render_as_grid_pngs_for_every_kind() {
    for kind in [
        ChartKind::Column,
        ChartKind::Bar,
        ChartKind::Line,
        ChartKind::Area,
        ChartKind::Pie,
        ChartKind::Scatter,
    ] {
        let mut sheet = sample(kind);
        if kind == ChartKind::Scatter {
            sheet.tabs[0].charts[0].data = "B1:D4".into();
        }
        let bytes = crate::sheet::render::png(
            &sheet,
            &sheet.tabs[0],
            "A1:M20".parse::<address::Range>().unwrap().bounds(),
            None,
        )
        .unwrap();
        assert!(bytes.starts_with(b"\x89PNG"), "{kind:?}");
    }
}

#[test]
fn sheet_chart_plot_uses_height_and_margins_follow_drawn_content() {
    let mut sheet = sample(ChartKind::Column);
    let short = plot(&render(&sheet));
    assert!(short[3] >= sheet.tabs[0].charts[0].h * 0.75, "{short:?}");
    assert!(short[1] <= 15.0);
    sheet.tabs[0].charts[0].title = Some("Revenue and costs".into());
    sheet.tabs[0].charts[0].x_title = Some("Reporting month".into());
    for row in 2..=4 {
        sheet.tabs[0]
            .cells
            .get_mut(&format!("A{row}"))
            .unwrap()
            .input = format!("Reporting month {row} (forecast)");
    }
    let svg = render(&sheet);
    let decorated = plot(&svg);
    assert!(svg.contains("rotate(45)"));
    assert!(decorated[1] > short[1]);
    let boxes = text_boxes(&sheet, &svg);
    let title = boxes
        .iter()
        .find(|(text, _)| text == "Revenue and costs")
        .unwrap();
    let top_tick = boxes.iter().find(|(text, _)| text == "$50,000").unwrap();
    assert!(title.1[3] + 4.0 <= top_tick.1[1]);
    assert!(
        sheet.tabs[0].charts[0].h - decorated[1] - decorated[3]
            > sheet.tabs[0].charts[0].h - short[1] - short[3]
    );
    sheet.tabs[0].charts[0].legend = Legend::Bottom;
    let bottom = plot(&render(&sheet));
    assert!(bottom[1] + bottom[3] < decorated[1] + decorated[3]);
}

#[test]
fn sheet_chart_bar_value_ticks_have_measured_gaps_and_categories_are_ellipsized() {
    let mut sheet = sample(ChartKind::Bar);
    sheet.tabs[0].cells.get_mut("A2").unwrap().input =
        "An extremely long category name that cannot fit beside the plot".into();
    let svg = render(&sheet);
    let bounds = plot(&svg);
    let boxes = text_boxes(&sheet, &svg);
    let mut ticks: Vec<_> = boxes
        .iter()
        .filter(|(text, b)| text.starts_with('$') && b[1] >= bounds[1] + bounds[3])
        .collect();
    assert!(ticks.len() >= 2, "value axis must retain meaningful ticks");
    ticks.sort_by(|a, b| a.1[0].total_cmp(&b.1[0]));
    for pair in ticks.windows(2) {
        assert!(
            pair[0].1[2] + 4.0 <= pair[1].1[0],
            "ticks overlap: {pair:?}"
        );
    }
    let category = boxes
        .iter()
        .find(|(text, _)| text.starts_with("An "))
        .unwrap();
    assert!(category.0.ends_with('…'));
    assert!(category.1[2] - category.1[0] <= sheet.tabs[0].charts[0].w * 0.4);
}

#[test]
fn sheet_chart_y_title_is_left_of_the_entire_tick_column() {
    let mut sheet = sample(ChartKind::Column);
    sheet.tabs[0].charts[0].y_title = Some("USD total".into());
    let svg = render(&sheet);
    let boxes = text_boxes(&sheet, &svg);
    let title = boxes.iter().find(|(text, _)| text == "USD total").unwrap();
    let tick_left = boxes
        .iter()
        .filter(|(text, _)| text.starts_with('$'))
        .map(|(_, b)| b[0])
        .fold(f64::INFINITY, f64::min);
    assert!(
        title.1[2] + 4.0 <= tick_left,
        "title {title:?}, tick left {tick_left}"
    );
}

#[test]
fn sheet_chart_pie_percentages_fit_inside_their_slices_and_fill_the_plot() {
    let sheet = sample(ChartKind::Pie);
    let svg = render(&sheet);
    let path = svg
        .split("<path d=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let (center, arc) = path.strip_prefix('M').unwrap().split_once('L').unwrap();
    let center: Vec<f64> = center
        .split_whitespace()
        .map(|n| n.parse().unwrap())
        .collect();
    let radius: f64 = arc
        .split_once('A')
        .unwrap()
        .1
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(radius >= 120.0, "radius is only {radius}");
    let fractions = [15000.0 / 85000.0, 25000.0 / 85000.0, 45000.0 / 85000.0];
    let boxes = text_boxes(&sheet, &svg);
    let percentages: Vec<_> = boxes
        .iter()
        .filter(|(text, _)| text.contains('%'))
        .collect();
    assert_eq!(percentages.len(), 3);
    for (index, (text, b)) in percentages.iter().enumerate() {
        assert!(
            text.strip_suffix('%').unwrap().parse::<f64>().is_ok(),
            "only percentages belong inside slices: {text}"
        );
        let start: f64 = fractions[..index].iter().sum::<f64>() * std::f64::consts::TAU;
        let end = start + fractions[index] * std::f64::consts::TAU;
        for (x, y) in [(b[0], b[1]), (b[0], b[3]), (b[2], b[1]), (b[2], b[3])] {
            let dx = x - center[0];
            let dy = y - center[1];
            assert!(dx.hypot(dy) < radius);
            let angle =
                (dy.atan2(dx) + std::f64::consts::FRAC_PI_2).rem_euclid(std::f64::consts::TAU);
            assert!(
                (start..=end).contains(&angle),
                "label {text} crosses slice edge"
            );
        }
    }
}

#[test]
fn sheet_chart_scatter_preserves_edge_markers_and_formats_nice_x_ticks() {
    let mut sheet = sample(ChartKind::Scatter);
    sheet.tabs[0].charts[0].data = "A1:B4".into();
    for (row, value) in [(2, "10000"), (3, "20000"), (4, "45000")] {
        let cell = sheet.tabs[0].cells.get_mut(&format!("A{row}")).unwrap();
        cell.input = value.into();
        cell.fmt.num = Some("$#,##0".into());
    }
    let svg = render(&sheet);
    let element = viewport(&svg);
    let bounds = plot(&svg);
    let view: Vec<f64> = attribute(element, "viewBox").map_or_else(
        || vec![0.0, 0.0, bounds[2], bounds[3]],
        |text| {
            text.split_whitespace()
                .map(|n| n.parse().unwrap())
                .collect()
        },
    );
    let mut count = 0;
    for circle in svg.split("<circle ").skip(1) {
        let x = number(circle, "cx");
        let y = number(circle, "cy");
        let radius = number(circle, "r");
        assert!(
            x - radius >= view[0] && x + radius <= view[0] + view[2],
            "x marker is clipped"
        );
        assert!(
            y - radius >= view[1] && y + radius <= view[1] + view[3],
            "y marker is clipped"
        );
        count += 1;
    }
    assert_eq!(count, 3);
    let boxes = text_boxes(&sheet, &svg);
    let ticks: Vec<_> = boxes
        .iter()
        .filter(|(_, b)| b[1] >= bounds[1] + bounds[3])
        .collect();
    assert!(ticks.len() >= 2);
    assert!(
        ticks.iter().all(|(text, _)| text.starts_with('$')),
        "{ticks:?}"
    );
}
