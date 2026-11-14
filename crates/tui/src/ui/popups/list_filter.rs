//! A typed filter over a list whose selection indexes the full list, so the
//! flows that act on the selection never see the filter.

/// Indices of the rows whose label contains `filter`, ignoring case.
pub fn matching<'a>(filter: &str, labels: impl IntoIterator<Item = &'a str>) -> Vec<usize> {
    let filter = filter.to_lowercase();
    labels
        .into_iter()
        .enumerate()
        .filter(|(_, label)| label.to_lowercase().contains(&filter))
        .map(|(index, _)| index)
        .collect()
}

/// Where the selection sits among the shown rows, if it is shown.
pub fn shown(rows: &[usize], selected: usize) -> Option<usize> {
    rows.iter().position(|&row| row == selected)
}

/// The selection after moving `delta` shown rows; a hidden selection lands on
/// the first shown row.
pub fn step(rows: &[usize], selected: usize, delta: i32) -> usize {
    let Some(&first) = rows.first() else {
        return selected;
    };
    let Some(at) = shown(rows, selected) else {
        return first;
    };
    rows[nexus_core::app::clamp_cursor(at, rows.len(), delta)]
}

/// The selection after the filter text changed: the first match, or unchanged
/// when nothing matches.
pub fn first(rows: &[usize], selected: usize) -> usize {
    rows.first().copied().unwrap_or(selected)
}

/// `8 skills`, or `2 of 8` while a filter narrows the list.
pub fn count(rows: usize, total: usize, filtering: bool, noun: &str) -> String {
    if filtering {
        format!("{rows} of {total}")
    } else {
        format!("{total} {noun}{}", if total == 1 { "" } else { "s" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_ignores_case_and_keeps_full_indices() {
        let rows = matching("PD", ["find-skills", "pdf", "deck", "pdf-review"]);
        assert_eq!(rows, vec![1, 3]);
        assert_eq!(matching("", ["a", "b"]), vec![0, 1]);
    }

    #[test]
    fn step_moves_among_shown_rows_and_clamps() {
        let rows = [1, 3, 6];
        assert_eq!(step(&rows, 1, 1), 3);
        assert_eq!(step(&rows, 3, 5), 6);
        assert_eq!(step(&rows, 3, i32::MIN / 2), 1);
        assert_eq!(step(&rows, 2, 1), 1, "a hidden selection snaps to the top");
        assert_eq!(step(&[], 4, 1), 4);
    }

    #[test]
    fn count_says_of_while_filtering() {
        assert_eq!(count(1, 1, false, "skill"), "1 skill");
        assert_eq!(count(2, 8, false, "app"), "8 apps");
        assert_eq!(count(2, 8, true, "app"), "2 of 8");
    }
}
