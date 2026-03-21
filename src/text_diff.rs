/// Render a simple line-based diff with explicit left/right labels.
pub(crate) fn render_text_diff(
    left_label: &str,
    right_label: &str,
    left_contents: &str,
    right_contents: &str,
) -> String {
    let mut rows = Vec::new();
    rows.push(format!("--- {left_label}"));
    rows.push(format!("+++ {right_label}"));
    let rendered_diff = diff_lines(left_contents, right_contents);
    if rendered_diff.is_empty() {
        rows.push(" (no description changes)".to_string());
    } else {
        rows.extend(rendered_diff);
    }
    rows.join("\n")
}

fn diff_lines(left: &str, right: &str) -> Vec<String> {
    let left_lines = left.lines().map(str::to_string).collect::<Vec<_>>();
    let right_lines = right.lines().map(str::to_string).collect::<Vec<_>>();
    let mut table = vec![vec![0usize; right_lines.len() + 1]; left_lines.len() + 1];

    for left_index in (0..left_lines.len()).rev() {
        for right_index in (0..right_lines.len()).rev() {
            table[left_index][right_index] = if left_lines[left_index] == right_lines[right_index] {
                table[left_index + 1][right_index + 1] + 1
            } else {
                table[left_index + 1][right_index].max(table[left_index][right_index + 1])
            };
        }
    }

    let mut rendered = Vec::new();
    let mut left_index = 0usize;
    let mut right_index = 0usize;
    while left_index < left_lines.len() && right_index < right_lines.len() {
        if left_lines[left_index] == right_lines[right_index] {
            rendered.push(format!(" {}", left_lines[left_index]));
            left_index += 1;
            right_index += 1;
        } else if table[left_index + 1][right_index] >= table[left_index][right_index + 1] {
            rendered.push(format!("-{}", left_lines[left_index]));
            left_index += 1;
        } else {
            rendered.push(format!("+{}", right_lines[right_index]));
            right_index += 1;
        }
    }
    while left_index < left_lines.len() {
        rendered.push(format!("-{}", left_lines[left_index]));
        left_index += 1;
    }
    while right_index < right_lines.len() {
        rendered.push(format!("+{}", right_lines[right_index]));
        right_index += 1;
    }

    rendered
}
