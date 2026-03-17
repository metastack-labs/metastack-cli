mod assets;
mod catalog;
mod issues;
mod resolution;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
mod workpad;

#[derive(Debug, Clone)]
pub struct LinearService<C> {
    pub(super) client: C,
    pub(super) default_team: Option<String>,
}

impl<C> LinearService<C> {
    pub fn new(client: C, default_team: Option<String>) -> Self {
        Self {
            client,
            default_team,
        }
    }
}
