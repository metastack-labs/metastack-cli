use super::model::Connection;

pub(super) struct CursorPager {
    limit: Option<usize>,
    page_size: usize,
    collected: usize,
    after: Option<String>,
    exhausted: bool,
}

impl CursorPager {
    pub(super) fn new(limit: Option<usize>, page_size: usize) -> Self {
        Self {
            limit,
            page_size,
            collected: 0,
            after: None,
            exhausted: false,
        }
    }

    pub(super) fn next_page_size(&self) -> Option<usize> {
        if self.exhausted {
            return None;
        }

        match self.limit {
            Some(limit) if self.collected >= limit => None,
            Some(limit) => Some(
                limit
                    .saturating_sub(self.collected)
                    .clamp(1, self.page_size),
            ),
            None => Some(self.page_size),
        }
    }

    pub(super) fn after(&self) -> Option<String> {
        self.after.clone()
    }

    pub(super) fn advance<T>(&mut self, connection: &Connection<T>) {
        self.collected += connection.nodes.len();

        let Some(page_info) = connection.page_info.as_ref() else {
            self.exhausted = true;
            self.after = None;
            return;
        };

        if !page_info.has_next_page {
            self.exhausted = true;
            self.after = None;
            return;
        }

        let Some(cursor) = page_info.end_cursor.clone() else {
            self.exhausted = true;
            self.after = None;
            return;
        };

        self.after = Some(cursor);
    }
}
