use demiurge_core::layout::tile;

const AREA: (i32, i32, u32, u32) = (0, 40, 2560, 1400);

#[test]
fn one_window_takes_the_work_area() {
    assert_eq!(tile(AREA, 1, 0.5), vec![AREA]);
    assert!(tile(AREA, 0, 0.5).is_empty());
}

#[test]
fn the_master_takes_its_ratio_and_the_stack_the_rest() {
    let tiles = tile(AREA, 3, 0.6);
    assert_eq!(tiles[0], (0, 40, 1536, 1400));
    assert_eq!(tiles[1], (1536, 40, 1024, 700));
    assert_eq!(tiles[2], (1536, 740, 1024, 700));
}

#[test]
fn the_last_slice_takes_what_the_division_leaves() {
    let tiles = tile((0, 0, 1000, 1000), 4, 0.5);
    let heights: Vec<u32> = tiles[1..].iter().map(|t| t.3).collect();
    assert_eq!(heights, vec![333, 333, 334]);
    assert_eq!(tiles[3].1, 666);
}
