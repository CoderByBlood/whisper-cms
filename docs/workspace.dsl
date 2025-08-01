workspace {

    model {
        admin = person admin "Manages content, sites, users, plugins, themes"
        su = person su "starts the system"

        WhisperCMS = softwareSystem WhisperCMS "Multi-site Rust CMS with plugin/theme system" {

            Core = container Core "The core" "Rust" "internal" {
                // PluginSPI = component PluginSPI "The spec all plugins must implement" "Rust" "interface"
                ThemeSPI = component ThemeSPI "The spec all themes must implement" "Rust" "interface"
                AdminAPI = component AdminAPI "The API for Admin UI" "Rust" "api"

                StaticService = component StaticService "Serve static content" "Rust" "service"
                // ConfigService = component ConfigService "Configuration stored in the content database" "Rust" "service"
                ContentService = component ContentService "Serves dynamic content" "Rust" "service"
                DataService = component DataService "Processes SQL, DML, & DDL" "Rust" "service"
                StartupManager = component StartupManager "Verifies WhisperCMS correctly starts" "Rust" "service"
                RequestManager = component RequestManager "Manages all content requests" "Rust" "service, router"
                ThemeManager = component ThemeManager "CRUDs themes from git" "Rust" "service"

                Pingora = component Pingora "For Statics" "Rust" "library"
                Git2 = component Git2 "In process git" "Rust" "library"
                Axum = component Axum "Content routing" "Rust" "library"
                Argon2 = component Argon2 "File encryption" "Rust" "library"

                su -> RequestManager "starts"

                RequestManager -> StartupManager "starts"
                StartupManager -> DataService "starts"
                StartupManager -> Argon2 "uses"
                //Cast -> RequestManager "binds"

                AdminAPI -> ThemeManager "loads"
                RequestManager -> StaticService "forwards"
                RequestManager -> ContentService "forwards"
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
                su -> this "starts"
                admin -> this "uses console securely" "https"
                this -> RequestManager "requests"
                RequestManager -> this "responds"
            }

            LibSQL = container LibSQL "LibSQL Database" "Rust" "external" {
                DataService -> LibSQL "calls"
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
        component Core WhisperCMS-Component-Core {
            include *
            autolayout lr
            title "WhisperCMS - Core - Component Diagram"
        }
        component AdminTheme WhisperCMS-Component-AdminTheme {
            include *
            autolayout lr
            title "WhisperCMS - AdminTheme - Component Diagram"
        }

        dynamic Core WhisperCMS-Startup-Sequence {
            su -> RequestManager "(cli arg)->(Ready)"
            RequestManager -> StartupManager "(cli arg)->(Settings)"
            RequestManager -> StartupManager "(Settings)->(Kernel)"
            RequestManager -> ContentService "(Kernel)->(Ready)"
            RequestManager -> su "successfully started"
            RequestManager -> Nginx "303 - /"
            Nginx -> admin "proxy response"
        }

        dynamic Core WhisperCMS-Installation-Sequence {
            su -> RequestManager "(cli arg)->(Ready)"
            RequestManager -> StartupManager "(cli arg)->(Settings | Config | None)"
            admin -> Nginx "GET /"
            Nginx -> RequestManager "request index.html"
            RequestManager -> Nginx "200 - login.html"
            Nginx -> admin "proxy response"
            admin -> Nginx "/POST /login {password}"
            Nginx -> RequestManager "do login"
            RequestManager -> StartupManager "(Password)->(Settings| Config | None)"
            RequestManager -> Nginx "200 - configuration.html"
            Nginx -> admin "proxy response"
            admin -> Nginx "POST /configure {form info}"
            Nginx -> RequestManager "save configuration"
            RequestManager -> StartupManager "(ConfigData)->(Settings | Config)"
            StartupManager -> DataService "(ConfigData)->(Config)"
            DataService -> LibSQL "(Statement)->(ResultSet)"
            RequestManager -> Nginx "200 - installation.html"
            Nginx -> admin "proxy response"
            admin -> Nginx "POST /install {site options}"
            Nginx -> RequestManager "do installation"
            RequestManager -> StartupManager "(SettingsData)->(Settings)"
            StartupManager -> DataService "(SettingData)->(Settings)"
            DataService -> LibSQL "(Statement)->(ResultSet)"
            RequestManager -> StartupManager "(Settings)->(Kernel)"
            RequestManager -> ContentService "(Kernel)->(Ready)"
            RequestManager -> su "successfully started"
            RequestManager -> Nginx "303 - /"
            Nginx -> admin "proxy response"
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