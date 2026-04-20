use demiurge::layout::Layout;

#[test]
fn layout_next_cycles() {
    assert_eq!(Layout::Floating.next(), Layout::Tile);
    assert_eq!(Layout::Tile.next(), Layout::Monocle);
    assert_eq!(Layout::Monocle.next(), Layout::Floating);
}

#[test]
fn layout_full_cycle() {
    let mut l = Layout::Floating;
    l = l.next(); // Tile
    l = l.next(); // Monocle
    l = l.next(); // Floating
    assert_eq!(l, Layout::Floating);
}

#[test]
fn layout_from_str() {
    assert_eq!(Layout::from_str("floating"), Layout::Floating);
    assert_eq!(Layout::from_str("tile"), Layout::Tile);
    assert_eq!(Layout::from_str("monocle"), Layout::Monocle);
    assert_eq!(Layout::from_str("unknown"), Layout::Floating);
    assert_eq!(Layout::from_str(""), Layout::Floating);
}
