//! Review comments on an agent's diff, and the prompt that sends them back to it.

/// A line of the diff a comment can attach to: a line of the new file, or a removed line
/// of the old one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Location {
    pub file: String,
    pub line: u32,
    pub removed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    pub location: Location,
    /// The diff line commented on, with its `+`, `-` or space.
    pub code: String,
    pub text: String,
}

/// The location of each line of a unified diff, `None` for headers.
/// Inside a hunk the header's line counts decide where it ends, so a content line such as
/// `+++x` is never taken for a file header.
pub fn locations(lines: &[String]) -> Vec<Option<Location>> {
    let (mut old_file, mut new_file) = (String::new(), String::new());
    let (mut old, mut new, mut old_left, mut new_left) = (0u32, 0u32, 0u32, 0u32);
    let mut found = Vec::with_capacity(lines.len());
    for line in lines {
        let in_hunk = old_left > 0 || new_left > 0;
        let file = || match new_file.as_str() {
            "" => old_file.clone(),
            file => file.to_string(),
        };
        let location = if in_hunk && line.starts_with('-') {
            old_left = old_left.saturating_sub(1);
            old += 1;
            Some(Location {
                file: file(),
                line: old - 1,
                removed: true,
            })
        } else if in_hunk && line.starts_with('+') {
            new_left = new_left.saturating_sub(1);
            new += 1;
            Some(Location {
                file: file(),
                line: new - 1,
                removed: false,
            })
        } else if in_hunk && line.starts_with(' ') {
            old_left = old_left.saturating_sub(1);
            new_left = new_left.saturating_sub(1);
            old += 1;
            new += 1;
            Some(Location {
                file: file(),
                line: new - 1,
                removed: false,
            })
        } else {
            if let Some(path) = line.strip_prefix("--- ") {
                old_file = strip_side(path, "a/");
                new_file.clear();
            } else if let Some(path) = line.strip_prefix("+++ ") {
                new_file = strip_side(path, "b/");
            } else if let Some((start_old, count_old, start_new, count_new)) = hunk(line) {
                (old, old_left, new, new_left) = (start_old, count_old, start_new, count_new);
            }
            None
        };
        found.push(location);
    }
    found
}

fn strip_side(path: &str, side: &str) -> String {
    match path {
        "/dev/null" => String::new(),
        path => path.strip_prefix(side).unwrap_or(path).to_string(),
    }
}

/// `@@ -a[,b] +c[,d] @@` as `(a, b, c, d)`, a missing count being 1.
fn hunk(line: &str) -> Option<(u32, u32, u32, u32)> {
    let ranges = line.strip_prefix("@@ -")?;
    let (old, rest) = ranges.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let range = |text: &str| -> Option<(u32, u32)> {
        match text.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((text.parse().ok()?, 1)),
        }
    };
    let ((a, b), (c, d)) = (range(old)?, range(new)?);
    Some((a, b, c, d))
}

/// One message asking the agent to address `comments`, in diff order.
pub fn prompt(comments: &[Comment]) -> String {
    let mut text = String::from("Review comments on your changes. Address each one:\n\n");
    for comment in comments {
        let place = &comment.location;
        let removed = if place.removed { " (removed line)" } else { "" };
        text.push_str(&format!(
            "- {}:{}{removed}: {}\n  {}\n",
            place.file, place.line, comment.text, comment.code
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    fn at(file: &str, line: u32, removed: bool) -> Option<Location> {
        Some(Location {
            file: file.into(),
            line,
            removed,
        })
    }

    #[test]
    fn hunk_lines_get_their_file_and_line_numbers() {
        let diff = lines(
            "diff --git a/src/a.rs b/src/a.rs\n\
             index 1..2 100644\n\
             --- a/src/a.rs\n\
             +++ b/src/a.rs\n\
             @@ -10,3 +10,4 @@ fn main() {\n \
             keep\n\
             -old\n\
             +new\n\
             +++counter\n \
             tail",
        );
        assert_eq!(
            locations(&diff),
            [
                None,
                None,
                None,
                None,
                None,
                at("src/a.rs", 10, false),
                at("src/a.rs", 11, true),
                at("src/a.rs", 11, false),
                at("src/a.rs", 12, false),
                at("src/a.rs", 13, false),
            ]
        );
    }

    #[test]
    fn new_deleted_and_second_files_are_tracked() {
        let diff = lines(
            "diff --git a/gone.rs b/gone.rs\n\
             deleted file mode 100644\n\
             --- a/gone.rs\n\
             +++ /dev/null\n\
             @@ -1 +0,0 @@\n\
             -bye\n\
             \\ No newline at end of file\n\
             diff --git a/new.rs b/new.rs\n\
             new file mode 100644\n\
             --- /dev/null\n\
             +++ b/new.rs\n\
             @@ -0,0 +1,2 @@\n\
             +one\n\
             +two",
        );
        let found = locations(&diff);
        assert_eq!(found[5], at("gone.rs", 1, true));
        assert_eq!(found[6], None);
        assert_eq!(found[12], at("new.rs", 1, false));
        assert_eq!(found[13], at("new.rs", 2, false));
    }

    #[test]
    fn the_prompt_lists_each_comment_with_its_place_and_code() {
        let comments = [
            Comment {
                location: Location {
                    file: "src/a.rs".into(),
                    line: 11,
                    removed: false,
                },
                code: "+new".into(),
                text: "name this better".into(),
            },
            Comment {
                location: Location {
                    file: "src/a.rs".into(),
                    line: 11,
                    removed: true,
                },
                code: "-old".into(),
                text: "why remove this?".into(),
            },
        ];
        assert_eq!(
            prompt(&comments),
            "Review comments on your changes. Address each one:\n\
             \n\
             - src/a.rs:11: name this better\n  \
               +new\n\
             - src/a.rs:11 (removed line): why remove this?\n  \
               -old\n"
        );
    }
}
