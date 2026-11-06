use thomas_engine::sessions::sessions_from_bars;

const BAR: i64 = 60_000;

#[test]
fn empty_input_gives_no_sessions() {
    assert!(sessions_from_bars(&[]).is_empty());
}

#[test]
fn single_bar_is_one_session_of_one_bar() {
    assert_eq!(sessions_from_bars(&[5 * BAR]), vec![(5 * BAR, 6 * BAR)]);
}

#[test]
fn contiguous_bars_form_one_session_ending_after_the_last_bar() {
    assert_eq!(sessions_from_bars(&[0, BAR, 2 * BAR]), vec![(0, 3 * BAR)]);
}

#[test]
fn a_gap_starts_a_new_session() {
    let night = 100 * BAR;
    assert_eq!(
        sessions_from_bars(&[0, BAR, night, night + BAR]),
        vec![(0, 2 * BAR), (night, night + 2 * BAR)]
    );
}

#[test]
fn a_missing_bar_inside_a_day_also_splits() {
    assert_eq!(
        sessions_from_bars(&[0, BAR, 3 * BAR]),
        vec![(0, 2 * BAR), (3 * BAR, 4 * BAR)]
    );
}
