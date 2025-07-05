workspace {

    model {
        admin = person admin "Manages content, sites, users, plugins, themes"

        WhisperCMS = softwareSystem WhisperCMS "Multi-site Rust CMS with plugin/theme system" {

            Kernel = container Kernel "The core" "Rust" "internal" {
                Core = component Core "The OS level process" "Rust" "application"
                // PluginSPI = component PluginSPI "The spec all plugins must implement" "Rust" "interface"
                ThemeSPI = component ThemeSPI "The spec all themes must implement" "Rust" "interface"
                AdminAPI = component AdminAPI "The API for Admin UI" "Rust" "api"

                StaticService = component StaticService "Serve static content" "Rust" "service"
                // ConfigService = component ConfigService "Configuration stored in the content database" "Rust" "service"
                ContentService = component ContentService "Serves dynamic content" "Rust" "service"
                DataService = component DataService "Processes SQL and DDL" "Rust" "service"
                StartupManager = component StartupManager "Verifies WhisperCMS correctly starts" "Rust" "service"
                RequestManager = component RequestManager "Manages all content requests" "Rust" "service, router"
                ThemeManager = component ThemeManager "CRUDs themes from git" "Rust" "service"

                Pingora = component Pingora "For Statics" "Rust" "library"
                Git2 = component Git2 "In process git" "Rust" "library"
                Axum = component Axum "Content routing" "Rust" "library"
                Argon2 = component Argon2 "File encryption" "Rust" "library"

                Core -> StartupManager "checks with"
                StartupManager -> DataService "starts"
                StartupManager -> Argon2 "uses"
                //Core -> RequestManager "binds"

                AdminAPI -> ThemeManager "loads"
                RequestManager -> StaticService "forwards"
                RequestManager -> ContentService "forwards"
                RequestManager -> Core "bound by"
                // AdminAPI -> ConfigService "uses"
                StaticService -> Pingora "uses"
                ThemeManager -> Git2 "uses"
                ContentService -> Axum "uses"
            }

            AdminTheme = container AdminTheme "Admin User Experience" "SPA" "ui, external" {
                AdminSPA = component AdminSPA "The Admin UI" "Javascript" "ui, external"

                this -> ThemeSPI "implements"
                this -> AdminAPI "calls"

                StaticService -> AdminSPA "serves"
                ThemeManager -> AdminTheme "installs"
            }

            Nginx = container Nginx "Nginx" "Reverse Proxy" "router, external" {
                admin -> this "uses console securely" "https"
                this -> RequestManager "requests"
                RequestManager -> this "responds"
            }

            PostgreSQL = container PostgreSQL "PostgreSQL Database Server" "C" "external" {
                DataService -> PostgreSQL "calls"
            }
        }
    }

    views {
        systemContext WhisperCMS WhisperCMS-Context {
            include *
            autolayout lr
            title "WhisperCMS - System Context"
        }

        container WhisperCMS WhisperCMS-Containers {
            include *
            autolayout lr
            title "WhisperCMS - Container Diagram"
        }
        component Kernel WhisperCMS-Component-Kernel {
            include *
            autolayout lr
            title "WhisperCMS - Kernel - Component Diagram"
        }
        component AdminTheme WhisperCMS-Component-AdminTheme {
            include *
            autolayout lr
            title "WhisperCMS - AdminTheme - Component Diagram"
        }
    
        styles {

            # People
            element "Person" {
              background #08427b
              color #ffffff
              shape Person
            }

            # Software Systems
            element "Software System" {
              background #1168bd
              color #ffffff
            }

            # Containers
            element "Container" {
              background #438dd5
              color #ffffff
            }

            # Components
            element "Component" {
              background #85bbf0
              color #000000
            }

            # Applications
            element "application" {
                shape Diamond
            }

            # Interfaces
            element "interface" {
                background "#6DB33F"
                color "#ffffff"
                shape Circle
            }

            # APIs
            element "api" {
                background "#6DB33F"
                color "#ffffff"
                shape Component
            }

            # Services
            element "service" {
              background #facc2e
              color #000000
              shape Hexagon
            }

            # UI
            element "ui" {
                shape WebBrowser
            }

            # Libraries
            element "library" {
              background #e67e22
              color #ffffff
              shape Folder
            }

            # External systems / elements
            element "external" {
                background "#999999"
                color "#ffffff"
            }

            # Routing
            element "router" {
                shape Pipe
            }
        }

        theme default
    }
}