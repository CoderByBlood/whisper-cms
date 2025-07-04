workspace {

    model {
        admin = person Admin "Manages content, sites, users, plugins, themes"

        WhisperCMS = softwareSystem WhisperCMS "Multi-site Rust CMS with plugin/theme system" {

            Kernel = container Kernel "The core" "Rust" "internal" {
                // PluginInterface = component PluginInterface "The spec all plugins must implement" "Rust" "interface"
                ThemeInterface = component ThemeInterface "The spec all themes must implement" "Rust" "interface"
                AdminAPI = component AdminAPI "The API for Admin UI" "Rust" "api"

                StaticService = component StaticService "Serve static content" "Rust" "service"
                SettingsService = component SettingsService "Configuration not stored in the content database" "Rust" "service"
                // OptionsService = component OptionsService "Configuration stored in the content database" "Rust" "service"
                SetupService = component SetupService "Initial set up for Whisper CMS" "Rust" "service"
                RequestManager = component RequestManager "Manages all content requests" "Rust" "service, router"
                ThemeManager = component ThemeManager "CRUDs themes from git" "Rust" "service"

                Pingora = component Pingora "Reverse Proxy" "Rust" "library"
                Git2 = component Git2 "In process git" "Rust" "library"

                SetupService -> ThemeManager "loads"
                RequestManager -> SettingsService "checks for settings"
                RequestManager -> SetupService "starts"
                RequestManager -> StaticService "forwards"
                // AdminAPI -> OptionsService "uses"
                StaticService -> Pingora "uses"
                ThemeManager -> Git2 "uses"
            }

            AdminTheme = container AdminTheme "Admin User Experience" "SPA" "ui, external" {
                AdminSPA = component AdminSPA "The Admin UI" "Javascript" "ui, external"

                this -> ThemeInterface "implements"
                this -> AdminAPI "calls"

                StaticService -> AdminSPA "serves"
                ThemeManager -> AdminTheme "installs"
            }

            Nginx = container Nginx "Nginx" "Reverse Proxy" "router, external" {
                admin -> this "uses console securely" "https"
                this -> RequestManager "requests"
                RequestManager -> this "responds"
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