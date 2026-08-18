use eh_layout::taffy;
use eh_layout::{Layout, Style};

#[test]
fn root_must_fill_available_space() {
    let mut lay = Layout::new();
    let tree = lay.tree_mut();
    let child = tree
        .new_leaf(Style {
            size: taffy::geometry::Size {
                width: taffy::Dimension::percent(0.5),
                height: taffy::Dimension::length(100.0),
            },
            ..Style::default()
        })
        .unwrap();
    let root = tree
        .new_with_children(
            Style {
                size: taffy::geometry::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Style::default()
            },
            &[child],
        )
        .unwrap();
    let res = tree.compute_layout(
        root,
        taffy::Size {
            width: taffy::style::AvailableSpace::Definite(1000.0),
            height: taffy::style::AvailableSpace::Definite(1000.0),
        },
    );
    println!("res ok={}", res.is_ok());
    let l = tree.layout(child).unwrap();
    println!("child: size={:?}", l.size);
    assert!(l.size.width > 0.0, "percent child should resolve against fill root");
}