use ingot_domain::item::{Classification, Priority};

use super::catalog::{DemoItem, DemoStack, DemoTemplate};

pub static FINANCE_TRACKER: DemoTemplate = DemoTemplate {
    slug: "finance-tracker",
    name: "Personal Finance Tracker",
    description: "Track accounts, categories, and transactions with a dashboard and CSV import.",
    color: "#6366f1",
    stacks: &[
        DemoStack {
            slug: "express-react",
            label: "Express + React",
            seed_readme: "\
# Personal Finance Tracker

Track income and expenses across multiple accounts.

## Tech Stack

- **Backend:** Express.js (server/) — port 3001, CORS enabled, JSON body parsing
- **Database:** SQLite via better-sqlite3
- **Frontend:** React + Vite (client/) — dev proxy forwarding /api to :3001

## Getting Started

```bash
# Backend
cd server && npm install && node index.js

# Frontend (separate terminal)
cd client && npm install && npx vite
```

## Conventions

- Routes go in server/routes/
- Database module: server/db.js (better-sqlite3, CREATE TABLE IF NOT EXISTS on import)
- React pages in client/src/pages/, components in client/src/components/
",
        },
        DemoStack {
            slug: "flask-react",
            label: "Flask + React",
            seed_readme: "\
# Personal Finance Tracker

Track income and expenses across multiple accounts.

## Tech Stack

- **Backend:** Flask (server/) — port 3001, flask-cors enabled, JSON responses
- **Database:** SQLite via Python sqlite3 module
- **Frontend:** React + Vite (client/) — dev proxy forwarding /api to :3001

## Getting Started

```bash
# Backend
cd server && pip install flask flask-cors && python app.py

# Frontend (separate terminal)
cd client && npm install && npx vite
```

## Conventions

- Routes go in server/routes/
- Database module: server/db.py (sqlite3, CREATE TABLE IF NOT EXISTS on connect)
- React pages in client/src/pages/, components in client/src/components/
",
        },
        DemoStack {
            slug: "rails-react",
            label: "Rails + React",
            seed_readme: "\
# Personal Finance Tracker

Track income and expenses across multiple accounts.

## Tech Stack

- **Backend:** Rails API mode (server/) — port 3001, rack-cors enabled
- **Database:** SQLite via ActiveRecord + sqlite3 gem
- **Frontend:** React + Vite (client/) — dev proxy forwarding /api to :3001

## Getting Started

```bash
# Backend
cd server && bundle install && rails db:migrate && rails server -p 3001

# Frontend (separate terminal)
cd client && npm install && npx vite
```

## Conventions

- Controllers in server/app/controllers/api/
- Models in server/app/models/
- Migrations in server/db/migrate/
- React pages in client/src/pages/, components in client/src/components/
",
        },
        DemoStack {
            slug: "go-react",
            label: "Go + React",
            seed_readme: "\
# Personal Finance Tracker

Track income and expenses across multiple accounts.

## Tech Stack

- **Backend:** Go net/http stdlib (server/) — port 3001, manual CORS headers
- **Database:** SQLite via modernc.org/sqlite
- **Frontend:** React + Vite (client/) — dev proxy forwarding /api to :3001

## Getting Started

```bash
# Backend
cd server && go run .

# Frontend (separate terminal)
cd client && npm install && npx vite
```

## Conventions

- HTTP handlers in server/handlers/
- Database module: server/db.go (modernc.org/sqlite, CREATE TABLE IF NOT EXISTS on init)
- React pages in client/src/pages/, components in client/src/components/
",
        },
        DemoStack {
            slug: "rails",
            label: "Rails",
            seed_readme: "\
# Personal Finance Tracker

Track income and expenses across multiple accounts.

## Tech Stack

- **Framework:** Ruby on Rails (full-stack) — port 3000
- **Database:** SQLite via ActiveRecord + sqlite3 gem
- **Views:** ERB templates with Rails built-in helpers
- **Frontend:** Import maps, Turbo, Stimulus (Rails defaults — no React, no Vite)

## Getting Started

```bash
bundle install
rails db:migrate
rails server
```

## Conventions

- Controllers in app/controllers/
- Models in app/models/
- Views in app/views/ (ERB templates)
- Migrations in db/migrate/
- Routes in config/routes.rb (RESTful resources)
- Use Rails form helpers, link_to, and Turbo for interactivity
",
        },
    ],
    items: &[
        DemoItem {
            title: "001 — Scaffold the project",
            description: "Set up the project structure following README.md for the chosen tech stack and conventions. The application should start successfully and render a placeholder page.",
            acceptance_criteria: "Application starts without errors | A placeholder page renders in the browser | README describes how to start the app",
            classification: Classification::Change,
            priority: Priority::Critical,
            labels: &["backend", "frontend"],
        },
        DemoItem {
            title: "002 — Data model for accounts, categories, and transactions",
            description: "Persist three entities in SQLite. An account has a name (required), type (required, one of: checking, savings, credit, cash), balance (defaults to 0), currency (defaults to USD), and created_at. A category has a name (required), type (required, one of: income, expense), icon, and created_at. A transaction has an account (required), category (required), amount (required), type (required, one of: income, expense), description, date (required), and created_at. The database should be initialized automatically on startup.",
            acceptance_criteria: "All three tables exist with correct columns and constraints | Account and category references on transactions are enforced | Type values are constrained",
            classification: Classification::Change,
            priority: Priority::Critical,
            labels: &["backend"],
        },
        DemoItem {
            title: "003 — Seed default categories on first run",
            description: "On first startup, if no categories exist, pre-populate 10 defaults: Salary (income), Freelance (income), Groceries (expense), Rent (expense), Utilities (expense), Transport (expense), Dining (expense), Entertainment (expense), Healthcare (expense), Shopping (expense). Each should have an emoji icon. Users can view all categories sorted by type then name. Subsequent startups should not create duplicates.",
            acceptance_criteria: "First startup seeds 10 categories | Categories are viewable sorted by type then name | Restarting does not duplicate them",
            classification: Classification::Change,
            priority: Priority::Major,
            labels: &["backend"],
        },
        DemoItem {
            title: "004 — Manage accounts",
            description: "Users can view a list of accounts sorted by name, showing name, type, and balance with currency. Users can create an account (name and type are required, balance defaults to 0) choosing a type from checking, savings, credit, or cash. Users can edit and delete accounts. Deleting an account that has transactions should be prevented with an explanation showing how many transactions reference it. The accounts view should be the default when the app loads, with a navigation header.",
            acceptance_criteria: "Accounts listed sorted by name | Creating without required fields shows errors | Type is selectable | Delete blocked when transactions exist | Accounts is the default view",
            classification: Classification::Change,
            priority: Priority::Critical,
            labels: &["backend", "frontend"],
        },
        DemoItem {
            title: "005 — Manage categories",
            description: "Users can view categories grouped into income and expense sections, showing each category's icon and name. Users can create a category (name and type required), edit, and delete. Deleting a category that has transactions should be prevented. Add a categories link to the navigation.",
            acceptance_criteria: "Categories grouped by income and expense | Icons display | Creating without required fields shows errors | Delete blocked when transactions exist | Navigation link works",
            classification: Classification::Change,
            priority: Priority::Major,
            labels: &["backend", "frontend"],
        },
        DemoItem {
            title: "006 — Record a transaction",
            description: "Users can record a transaction by entering an amount, choosing income or expense, selecting a date, picking an account, picking a category (only categories matching the selected income/expense type should appear), and optionally adding a description. All fields except description are required. Recording a transaction updates the associated account's balance (adding for income, subtracting for expense). The form clears after successful submission. Add a transactions link to the navigation.",
            acceptance_criteria: "All required fields validated before submission | Category selector filters to match income/expense type | Account balance updates after recording | Form clears on success | Navigation link works",
            classification: Classification::Change,
            priority: Priority::Critical,
            labels: &["backend", "frontend"],
        },
        DemoItem {
            title: "007 — Transaction list with filters",
            description: "Users can view transactions sorted by date (newest first), showing date, description, category (with icon), account name, and amount visually distinguished for income versus expense. Users can filter by date range, account, category, and income/expense type. Filters are combinable and clearable. An empty state appears when no transactions match.",
            acceptance_criteria: "Transactions sorted newest first | Income and expense amounts visually distinct | All filters work individually and combined | Filters clearable | Empty state when no matches",
            classification: Classification::Change,
            priority: Priority::Major,
            labels: &["backend", "frontend"],
        },
        DemoItem {
            title: "008 — Edit and delete transactions",
            description: "Users can edit a transaction with all fields pre-filled and delete a transaction with confirmation. Editing recalculates the account balance correctly (reverses the old amount, applies the new). Deleting reverses the balance effect. The transaction list and account balance update without a full page reload. Editing or deleting a non-existent transaction shows a not-found state.",
            acceptance_criteria: "Edit pre-fills all fields and recalculates balance | Delete reverses balance and asks for confirmation | List updates without reload | Not-found state for missing transactions",
            classification: Classification::Change,
            priority: Priority::Major,
            labels: &["backend", "frontend"],
        },
        DemoItem {
            title: "009 — Account detail with transaction history",
            description: "Clicking an account name navigates to a detail page showing account info (name, type, balance, currency) and computed totals (total income, total expenses), followed by a list of only the transactions for that account. A back link returns to the accounts list. Viewing a non-existent account shows a not-found state.",
            acceptance_criteria: "Detail page shows account info with computed totals | Transaction list filtered to that account | Back link works | Non-existent account shows not-found",
            classification: Classification::Change,
            priority: Priority::Major,
            labels: &["frontend", "backend"],
        },
        DemoItem {
            title: "010 — Dashboard with summary and net worth",
            description: "A dashboard replaces the accounts list as the default view. It shows four summary cards: net worth (sum of all account balances), monthly income, monthly expenses, and monthly net. Users can navigate between months with previous/next controls. All amounts are formatted with currency.",
            acceptance_criteria: "Dashboard is the default view | Net worth reflects all account balances | Monthly totals are correct | Month navigation works | Amounts formatted with currency",
            classification: Classification::Change,
            priority: Priority::Major,
            labels: &["frontend", "backend"],
        },
        DemoItem {
            title: "011 — Spending breakdown by category",
            description: "Below the dashboard summary, show categories ranked by total spending with proportional bars, amounts, and percentages. Users can toggle between an expense view and an income view. Percentages should sum to approximately 100%.",
            acceptance_criteria: "Categories sorted by total | Bars proportional to amounts | Expense/income toggle works | Percentages sum to ~100%",
            classification: Classification::Change,
            priority: Priority::Minor,
            labels: &["frontend", "backend"],
        },
        DemoItem {
            title: "012 — Import transactions from CSV",
            description: "Users can import transactions by uploading a CSV file with columns: date, description, amount, type, account_name, category_name. Account and category names are matched case-insensitively. Account balances are updated for each successfully imported transaction. Rows with missing required fields or unmatched account/category names are skipped. After import, show the number of successfully imported transactions and a list of skipped rows with reasons.",
            acceptance_criteria: "Valid CSV rows import correctly | Account balances updated | Unmatched names skipped with reason | Missing fields skipped with reason | Import results displayed",
            classification: Classification::Change,
            priority: Priority::Minor,
            labels: &["backend", "frontend", "data-import"],
        },
    ],
};
