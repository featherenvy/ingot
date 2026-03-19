use ingot_domain::item::{Classification, Priority};

pub struct DemoTemplate {
    pub slug: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub color: &'static str,
    pub stacks: &'static [DemoStack],
    pub items: &'static [DemoItem],
}

pub struct DemoStack {
    pub slug: &'static str,
    pub label: &'static str,
    pub seed_readme: &'static str,
}

pub struct DemoItem {
    pub title: &'static str,
    pub description: &'static str,
    pub acceptance_criteria: &'static str,
    pub classification: Classification,
    pub priority: Priority,
    pub labels: &'static [&'static str],
}

impl DemoTemplate {
    pub fn find_stack(&self, slug: &str) -> Option<&DemoStack> {
        self.stacks.iter().find(|s| s.slug == slug)
    }
}

pub static DEMO_CATALOG: &[&DemoTemplate] = &[
    &super::mini_crm::MINI_CRM,
    &super::finance_tracker::FINANCE_TRACKER,
];

pub fn find_template(slug: &str) -> Option<&'static DemoTemplate> {
    DEMO_CATALOG.iter().find(|t| t.slug == slug).copied()
}
