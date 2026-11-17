use super::*;

#[test]
fn image_looking_code_is_not_removed_or_loaded() {
    let source = "```mermaid\nflowchart LR\nA[\"![label](private.png)\"] --> B\n```";
    assert_eq!(strip_markdown_images(source), source);
    assert!(markdown_images(source).is_empty());
    assert!(markdown_images("`![literal](private.png)`").is_empty());
}

#[test]
fn real_images_are_removed_without_losing_surrounding_text() {
    let source = "Before ![plot](plot.png) after";
    assert_eq!(strip_markdown_images(source), "Before  after");
    let refs = markdown_images(source);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].1, "plot.png");
    assert_eq!(refs[0].2, "plot");
}
