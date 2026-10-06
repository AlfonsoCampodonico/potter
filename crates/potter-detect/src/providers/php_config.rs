//! Generated nginx and php-fpm configuration (spec §6.1.1). Paths are potter-owned so they don't
//! depend on how a catalog layer lays out /etc.

pub(crate) const NGINX_PATH: &str = "/etc/potter/nginx.conf";
pub(crate) const FPM_PATH: &str = "/etc/potter/php-fpm.conf";
pub(crate) const FPM_SOCKET: &str = "/run/php-fpm.sock";

const NGINX_TEMPLATE: &str = r#"worker_processes auto;
pid /run/nginx.pid;
error_log /dev/stderr warn;
daemon off;
user www-data;

events {
    worker_connections 1024;
}

http {
    include /etc/nginx/mime.types;
    default_type application/octet-stream;
    access_log /dev/stdout;
    server_tokens off;
    sendfile on;
    client_max_body_size 100m;
    gzip on;
    gzip_types text/plain text/css text/xml application/json application/javascript application/xml image/svg+xml;

    server {
        listen @PORT@;
        root @DOCROOT@;
        index index.php index.html;

        location ~ /\.(?!well-known) {
            deny all;
        }

        location / {
            try_files $uri $uri/ /index.php?$query_string;
        }

        location ~ \.php$ {
            try_files $uri =404;
            fastcgi_pass unix:@SOCKET@;
            fastcgi_index index.php;
            include /etc/nginx/fastcgi_params;
            fastcgi_param SCRIPT_FILENAME $realpath_root$fastcgi_script_name;
            fastcgi_param DOCUMENT_ROOT $realpath_root;
        }
    }
}
"#;

const FPM_TEMPLATE: &str = r#"[global]
error_log = /proc/self/fd/2
daemonize = no

[www]
user = www-data
group = www-data
listen = @SOCKET@
listen.owner = www-data
listen.group = www-data
listen.mode = 0660
pm = dynamic
pm.max_children = ${PHP_FPM_MAX_CHILDREN}
pm.start_servers = 2
pm.min_spare_servers = 1
pm.max_spare_servers = 3
clear_env = no
catch_workers_output = yes
decorate_workers_output = no
php_admin_value[opcache.validate_timestamps] = 0
"#;

/// `docroot` must already be validated (absolute, [A-Za-z0-9._/-] only).
pub(crate) fn nginx(port: u16, docroot: &str) -> String {
    NGINX_TEMPLATE
        .replace("@PORT@", &port.to_string())
        .replace("@DOCROOT@", docroot)
        .replace("@SOCKET@", FPM_SOCKET)
}

pub(crate) fn fpm() -> String {
    FPM_TEMPLATE.replace("@SOCKET@", FPM_SOCKET)
}
