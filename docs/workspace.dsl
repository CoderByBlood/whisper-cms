workspace {

    model {

        # People
        admin = person Admin "Configures site, manages extensions, publishes changes" "Person,Admin"
        author = person Author "Creates and edits content locally" "Person,Author"
        visitor = person Visitor "Browses published site content" "Person,Visitor"

        # Core System
        WhisperCMS = softwareSystem WhisperCMS "Rust-based content engine with strict boundaries and Git as the system-of-record" "WhisperCMS,System" {

            # ===== Authoring & Administration =====
            DesktopAdmin = container DesktopAdmin "Local-first admin app (Tauri + Leptos). Uses a custom 'wcms:' scheme to call the Rust backend." "Tauri, Leptos, Rust" "Container,Desktop" {
                Desktop_UI = component Desktop_UI "Admin UI (WebView)" "Leptos" "Component,Adapter,Inbound"
                Desktop_AppServices = component Desktop_AppServices "Admin Application Services (configure, preview, publish)" "Rust" "Component,Application"
                Desktop_GitAdapter = component Desktop_GitAdapter "Git Adapter (commit/push/pull; admin/preview/main)" "git2-rs/CLI" "Component,Adapter,Outbound"
                Desktop_FSAdapter = component Desktop_FSAdapter "Filesystem Adapter (settings.toml, config.toml, local repo)" "Filesystem" "Component,Adapter,Outbound"
                Desktop_MarketplaceClient = component Desktop_MarketplaceClient "Marketplace Client (query extension metadata)" "HTTP" "Component,Adapter,Outbound,Optional"

                # Hexagonal dependencies (inward)
                Desktop_UI -> Desktop_AppServices "Invokes admin commands via wcms: protocol" "wcms://"
                Desktop_AppServices -> Desktop_GitAdapter "Commit/push/pull changes" "SSH/HTTPS"
                Desktop_AppServices -> Desktop_FSAdapter "Read/write local config & repo state"
                Desktop_AppServices -> Desktop_MarketplaceClient "Query extension metadata" "HTTPS"
            }

            # ===== Publishing & Delivery =====
            Server = container Server "Hosts themes, serves content, dispatches events to plugins" "Rust, Pingora, Axum" "Container,Server" {
                # Inbound adapters
                IngressController = component IngressController "Ingress Controller (CORS, methods, rate limits)" "Pingora" "Component,Adapter,Inbound"
                WebServer = component WebServer "HTTP Router (maps routes to active Theme handlers)" "Axum" "Component,Adapter,Inbound"

                # Application/use-case core
                EventBus = component EventBus "Reactive Event Bus (request/content/response events)" "leptos_reactive" "Component,Application"
                ThemeEngine = component ThemeEngine "Theme Engine (executes commands, renders templates)" "Minijinja, lol_html" "Component,Application"
                PluginRuntime = component PluginRuntime "Plugin Runtime (sandbox; untrusted, capped resources)" "Rhai" "Component,Adapter,Untrusted"

                # Outbound adapters
                GitAdapter = component GitAdapter "Content Repository Adapter (read content/config from Git)" "git2-rs" "Component,Adapter,Outbound"
                SQLiteAdapter = component SQLiteAdapter "Persistence Adapter (local cache & indexing)" "SQLite/LibSQL" "Component,Adapter,Outbound"
                PolicyEngine = component PolicyEngine "Policy & Authorization (Cedar)" "Cedar" "Component,Adapter,Outbound,Security"
                Secrets = component Secrets "Secrets Management (credentials, keys)" "ROPS" "Component,Adapter,Outbound,Security"

                # Hexagonal dependencies (inward)
                IngressController -> WebServer "Forwards validated HTTP traffic" "HTTP"
                WebServer -> EventBus "Emit request events"
                EventBus -> PluginRuntime "Dispatch plugin effects (bounded)"
                EventBus -> ThemeEngine "Provide aggregated plugin proposals"
                ThemeEngine -> SQLiteAdapter "Query structured content/metadata"
                ThemeEngine -> GitAdapter "Read template assets/content if needed"
                PluginRuntime -> PolicyEngine "Check capabilities / policies"
                PluginRuntime -> Secrets "Access secrets when required"
                ThemeEngine -> Secrets "Access secrets when required"
            }

            # Internal data store (modeled as a container)
            ContentDB = container ContentDB "Local read-optimized cache for structured content and indexes" "SQLite/LibSQL" "Container,Database"


            # Container-to-container wiring
            Server -> ContentDB "Reads/Writes cached content & indexes" "SQL"
        }

        # External Systems
        GitRepo = softwareSystem GitRepo "System-of-record for content, configuration, and extensions" "External,System,Git" {
            # Internal data store (modeled as a container)
        }
        VisitorDesktop = softwareSystem VisitorDesktop "The browsers that visitors use from their desktops" "Browser,Windows,MacOS,iOS,Android"

        
        Marketplace = softwareSystem Marketplace "Catalog of plugin/theme manifests (optional)" "External,System,Marketplace,Optional"

        # People ↔ Systems
        admin -> DesktopAdmin "Configures site, manages extensions" "Local UI"
        author -> DesktopAdmin "Creates and edits content" "Local UI"
        visitor -> Server "Requests pages and assets" "HTTPS"

        # Systems ↔ External Systems
        DesktopAdmin -> GitRepo "Commit & push admin/preview/main branches" "SSH/HTTPS"
        Server -> GitRepo "Clone/Pull latest commits on startup/interval" "SSH/HTTPS"
        DesktopAdmin -> Marketplace "Search extension metadata" "HTTPS" "Optional"

        deploymentEnvironment Server {

            GitServer = deploymentNode "Git Remote" "GitHub/Gitea/self-hosted" {
                softwareSystemInstance GitRepo
            }

            Edge = deploymentNode "Public Internet" "Network" {
                softwareSystemInstance VisitorDesktop
            }

            ProdServer = deploymentNode "WhisperCMS Server Node" "Linux VM/Container/Bare metal" {
                deploymentNode "Content Delivery Server" "Runtime container for WhisperCMS" "Rust binary" {
                    containerInstance Server
                    containerInstance ContentDB
                }
                deploymentNode "ContentDB" "Content database" "SQLite"
            }

            # Runtime relationships
            ProdServer -> GitServer "Pull" "SSH/HTTPS"
            Edge -> ProdServer "HTTPS traffic from visitors" "443/TLS"
        }

        deploymentEnvironment Desktop {

            # Top-level runtime environment
            AdminMachine = deploymentNode "Admin Workstation" "Windows/macOS/Linux" {
                # Could host AdminTheme SPA locally if desired
                containerInstance DesktopAdmin
            }

            GitRemote = deploymentNode "Git Remote" "GitHub/Gitea/self-hosted" {
                softwareSystemInstance GitRepo
            }

            # Runtime relationships
            AdminMachine -> GitRemote "Push/Pull" "SSH/HTTPS"

        }
    }

    views {

        # System Landscape (high-level)
        systemContext WhisperCMS WhisperCMS-Context {
            include *
            autolayout lr
            title "WhisperCMS - System Context"
        }

        # Container view
        container WhisperCMS WhisperCMS-Containers {
            include *
            autolayout lr
            title "WhisperCMS - Container Diagram"
        }

        # Component views (Hexagonal)
        component Server WhisperCMS-Component-Server {
            include *
            autolayout lr
            title "WhisperCMS - Server - Component Diagram"
        }

        component DesktopAdmin WhisperCMS-Component-DesktopAdmin {
            include *
            autolayout lr
            title "WhisperCMS - DesktopAdmin - Component Diagram"
        }
        deployment WhisperCMS Server WhisperCMS-Deployment-Server {
            include *
            autolayout lr
            title "WhisperCMS - Deployment - Server"
        }

        deployment WhisperCMS Desktop WhisperCMS-Deployment-Desktop {
            include *
            autolayout lr
            title "WhisperCMS - Deployment - Desktop"
        }

        # Styles (template style)
        styles {
            element "Person" {
                background #08427b
                color #ffffff
                shape Person
            }
            element "Software System" {
                background #1168bd
                color #ffffff
            }
            element "Container" {
                background #438dd5
                color #ffffff
            }
            element "Component" {
                background #85bbf0
                color #000000
            }
            element "application" {
                shape Diamond
            }
            element "interface" {
                background "#6DB33F"
                color "#ffffff"
                shape Circle
            }
            element "api" {
                background "#6DB33F"
                color "#ffffff"
                shape Component
            }
            element "service" {
                background #facc2e
                color #000000
                shape Hexagon
            }
            element "ui" {
                shape WebBrowser
            }
            element "library" {
                background #e67e22
                color #ffffff
                shape Folder
            }
            element "external" {
                background "#999999"
                color "#ffffff"
            }
            element "router" {
                shape Pipe
            }
        }

        theme default
    }
}
