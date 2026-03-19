use ingot_domain::item::{Classification, Priority};

use super::catalog::{DemoItem, DemoStack, DemoTemplate};

pub static MINI_CRM: DemoTemplate = DemoTemplate {
    slug: "mini-crm",
    name: "Mini CRM",
    description: "A small CRM with companies and contacts — CRUD, detail pages, and search.",
    color: "#10b981",
    stacks: &[
        DemoStack {
            slug: "express-react",
            label: "Express + React",
            seed_readme: "\
# Mini CRM

A small CRM to manage companies and contacts.

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
# Mini CRM

A small CRM to manage companies and contacts.

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
# Mini CRM

A small CRM to manage companies and contacts.

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
# Mini CRM

A small CRM to manage companies and contacts.

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
# Mini CRM

A small CRM to manage companies and contacts.

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
            priority: Priority::Major,
            labels: &["backend", "frontend"],
        },
        DemoItem {
            title: "002 — Data model for companies and contacts",
            description: "Persist two entities in SQLite. A company has a name (required), domain, industry, and created_at timestamp. A contact has first_name (required), last_name (required), email (required, unique), phone, role, created_at, and belongs to a company. The database should be initialized automatically on startup.",
            acceptance_criteria: "Both tables exist with correct columns and constraints | A contact's company reference is enforced | Duplicate emails are rejected",
            classification: Classification::Change,
            priority: Priority::Critical,
            labels: &["backend"],
        },
        DemoItem {
            title: "003 — Manage companies",
            description: "Users can view a list of companies sorted by name, create a new company (name is required — show a validation error if missing), edit an existing company's details, and delete a company. The companies list should be the default view when the app loads.",
            acceptance_criteria: "Companies listed in alphabetical order | Creating without a name shows an error | Edit pre-fills current values | Delete removes the company | Companies list is the default view",
            classification: Classification::Change,
            priority: Priority::Major,
            labels: &["backend", "frontend"],
        },
        DemoItem {
            title: "004 — Manage contacts",
            description: "Users can view a list of contacts showing name, email, phone, company name, and role, sorted by last name. Users can create a contact (first name, last name, and email are required) choosing a company from a selector. Users can edit and delete contacts. Navigation should link between the companies and contacts views.",
            acceptance_criteria: "Contacts listed with company name shown | Creating without required fields shows errors | Company selector lists existing companies | Navigation works between companies and contacts",
            classification: Classification::Change,
            priority: Priority::Major,
            labels: &["backend", "frontend"],
        },
        DemoItem {
            title: "005 — Company detail with associated contacts",
            description: "Clicking a company name navigates to a detail page showing the company's name, domain, and industry, followed by a list of only the contacts belonging to that company. A back link returns to the companies list. Viewing a non-existent company shows a not-found state.",
            acceptance_criteria: "Clicking a company navigates to its detail page | Company info and filtered contacts are shown | Back link works | Non-existent company shows not-found",
            classification: Classification::Change,
            priority: Priority::Major,
            labels: &["frontend", "backend"],
        },
        DemoItem {
            title: "006 — Search and filter contacts",
            description: "Users can search contacts by name or email using a text input, and filter by company using a selector. Both filters can be combined and cleared independently. An empty state message appears when no contacts match.",
            acceptance_criteria: "Searching by name or email narrows the list | Filtering by company narrows the list | Filters combine | Each filter can be cleared | Empty state shown when no matches",
            classification: Classification::Change,
            priority: Priority::Minor,
            labels: &["frontend", "backend"],
        },
    ],
};
