# WhisperCMS Architecture

WhisperCMS is a Rust-powered, plugin-based, multi-site Content Management System
with a modern, WordPress-inspired model.

---

## 📌 High-level Architecture

```text
+----------------------+
|     Nginx Reverse    |
|        Proxy         |
+----------+-----------+
           |
+----------v-----------+
|     Rust Server      |
|    (whisper-cms)     |
|----------------------|
| - Core server        |
| - Themes SSR         |
| - Plugin system      |
+---+--------------+---+
    |              |
+---v--------+ +---v---+--+
|   Redis    | | Postgres |
|   Cache    | | Database |
| (Optional) | |          |
+------------+ +----------+
```

---

## 📌 Components

### ✅ Nginx

- TLS termination
- Reverse proxy to Rust server
- Static asset serving

### ✅ Rust Server

- Cargo workspace
- Core server
- SSR themes
- SPA themes
- Plugin architecture
- REST / JSON API
- Admin console (SPA)
- ApplicationContext (static/global)
- RequestContext (task-local scoped)

### ✅ PostgreSQL

- Sites, Users, Roles
- Posts, Terms, Comments
- Options
- Audit log

### ✅ Redis (optional)

- [ ] TODO: Complete this section

---

## 📌 Plugin & Theme Discovery

- Discoverable via crates.io
- Plugins expose settings schema via Rust function
