<!-- markdownlint-disable MD024 -->

# WhisperCMS Design Decisions

This document explains key design decisions for the project.

---

## Motivation

WhisperCMS aims to provide users a safe, fast, and flexible general purpose
content management system using newer proven technologies and uses resources
effectively for small and large deployments.

---

## Design Priorities (Ranked)

1. **Safety:** The CMS should leverage as much static compile-time checking as
   possible
2. **Performance:** The CMS should be lightning fast with deterministic
   performance without compromising safety
3. **User Experience:** The CMS should be highly usable from author, editor, and
   administrator perspectives without compromising performance or safety
4. **Developer Experience:** The CMS should be easy to extend without
   compromising the user experience, performance, or safety

---

## Programming Language

### Decision: Only rust will be used on the server

**Rationale:** Rust avoids inherent safety concerns that plague many scripting
languages that allow dynamic execution of code.

#### Alternatives Considered

- PHP/Python: Inherent safety concerns with `eval` and dynamic SQL
- Node.js: The second priority is performance and Node.js will be slower
- Java/Go: Although comparable performance, the garbage collection causes the
  performance to be non-deterministic

#### Tradeoffs

- Compiled binaries: Need an approach to handle dynamic aspects like plugins,
  themes, etc.
- Compatible platforms: Although rust is compatible or many platforms and OSes
  it not as prolific yet as older languages

---

## Extensibility

### Decision: There will be plugins and themes which will use the marketplace metaphor

**Rationale:** This approach has proven very successful for other CMSs like
Wordpress

#### Alternatives Considered

- Inline all functionality: This is contrary to the motivation of providing a
  general purpose CMS

#### Tradeoffs

- Stable APIs: 3rd Party developers will need stable APIs across versions
- Loading plugins and themes: Need an approach the doesn't compromise safety

---

## Plugin and Theme Loading

### Decision: Plugins and themes will be statically compiled

**Rationale:** Nothing beats the safety of statically compiled rust

#### Alternatives Considered

- Use scripting: Although rust has scripting capabilities it would compromise
  safety and performance
- Dynamic library loading: Although rust can dynamically load libraries, this
  would compromise safety without any improvement in performance

#### Tradeoffs

- Rebuilding binaries: Adding plugin or a theme will require a build cycle
- Admin experience: A rebuild will require a restart of the server which will
  interrupt the flow of administering the CMS

---

## Plugin and Theme Marketplace

### Decision: Plugins and themes will be hosted at crates.io with naming conventions

**Rationale:** The infrastructure for the marketplace already exists at
crates.io and using crates.io with naming conventions for curating crates is the
rust way

#### Alternatives Considered

- WhisperCMS specific repository: Significant effort required that effectively
  recreate crates.io

#### Tradeoffs

- Tooling: Using crates.io gains the native tooling from the language
- Safety: Hosting a separate repository would increase safety if code reviews
  were part of the approval and update processes; however, given the open source
  nature of WhisperCMS that is not a responsibility the project can undertake

---

## Plugin and Theme API

### Decision: Plugins and themes will provide their configuration through APIs

**Rationale:** This enables static compile-time typing for updatable
configuration

#### Alternatives Considered

- Use config files: This would require internal filesystem access to read the
  file which compromises safety and performance
- Use database tables: This would create a circular dependency for a plugin to
  access the DB while being loaded

#### Tradeoffs

- Adds surface to API: This API will need to be stable across versions so it
  must be carefully thought through

---

## Multi-Site

### Decision: Multi-site will be built-in not bolted-on through a plugin

**Rationale:** Given the prevalence of the Cloud and the desire to optimize
resources, this is a very common use-case as should be part of the core

#### Alternatives Considered

- Single-site default: One site is automatically accommodated in a multi-site
  configuration

#### Tradeoffs

- Database complexity: Need additional table(s) and foreign keys
- Performance: The additional table and foreign keys will impact queries that
  require joins when in a single-site configuration
- Admin user experience: Need additional level of permissions for site admins vs
  platform (super) admins
- Developer experience: To avoid compromising the user experience, the admin
  console must detect single- vs multi-site configuration

---

## Database

### Decision: PostgreSQL Database is the datastore for structured data

**Rationale:** PostgreSQL's mature and scalable functionality around full text
searching, native JSON storage and indexing, compile-time checked rust drivers,
and cryptography most closely aligns with the project's priorities.

#### Alternatives Considered

- MySQL: Although is performant, PostgreSQL is faster at scale
- SQLite: Missing key features and has scaling limitations
- MongoDB: CMS's have relational- and document-based requirements, given the
  heavy querying for rendering and administering, a relational database is
  better suited
- Oracle: just kidding

#### Tradeoffs

- Vendor Lock: Leveraging PostgreSQL-specific extensions severely limits the
  ability to migrate to another database
- Availability: Users must use PostgreSQL and cannot use another database

---

## Auditing

### Decision: Use hard deletes with a full audit trail of who, when and what

**Rationale:** PostgreSQL provides an automated way to create full audit of data
changes so there is no need to take on the storage costs and performance hit for
soft deletes

#### Alternatives Considered

- Soft Deletes: Adding a deleted_at column to tables adds unnecessary bloat

#### Tradeoffs

- Difficult Undo Delete/Restore/Recycle-Bin Implementation: Requires finding the
  change in the audit table and unpacking it to undo/restore deletes

---

## Scope and Impact of Configuration Changes

### Decision: Configuration changes will take effect only for new incoming requests

**Rationale:** Having configuration changes affect executing requests is
non-deterministic

#### Alternative Considered

- Instantaneous updates: Although requires less memory, would require locking
  and possibly negatively impacting performance

#### Tradeoffs

- Memory: Ensuring requests gets a copy of configuration requires more memory

---

## Admin UI

### Decision: The Admin interface will be a SPA (Single Page Application)

**Rationale:** The admin console should be highly usable (read reactive) and
should not support headless crawling

#### Alternatives Considered

- SSR: Server Side Rendering allow headless crawling and compromises the
  developer experience to create a reactive user experience

#### Tradeoffs

- Compatibility: SSR is more widely supported across all platforms but
  compatibility is not a priority; however, user experience and developer are
  priorities

---

## Internationalization (i18n) and Localization (l10n)

### Decision: I18n and l10n will be built-in not bolted-on through a plugin

**Rationale:** Serving content in multiple languages is required for many
applications and is a basic function of a CMS

#### Alternatives Considered

- Plugin: Given how tied translations are to the content a plugin would require
  unnecessary overhead for multilingual applications

#### Tradeoffs

- Performance: Translation resolution is not free and will need a solution that
  minimizes the performance impact for single language applications

<!-- TODO: Revisit when necessary
## Caching Strategy

- In-process cache for simplicity
- Redis optional for scale
- Pub/Sub invalidation

---
-->
