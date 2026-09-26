//! The web engine behind the browser: addresses, HTTP(S), HTML and
//! layout. Drawing and input live in `gui::browser`.

pub mod html;
pub mod http;
pub mod layout;
pub mod url;

use alloc::format;
use alloc::string::String;

use html::Document;
use url::Url;

pub const HOME: &str = "about:home";

/// A loaded page.
pub struct Page {
    /// None for built-in pages.
    pub url: Option<Url>,
    pub doc: Document,
}

impl Page {
    pub fn address(&self) -> String {
        match &self.url {
            Some(u) => format!("{}", u),
            None => String::from(HOME),
        }
    }

    /// Resolve a link on this page.
    pub fn resolve(&self, link: &str) -> Option<Url> {
        match &self.url {
            Some(u) => {
                let base = match &self.doc.base {
                    Some(b) => u.join(b).unwrap_or_else(|| u.clone()),
                    None => u.clone(),
                };
                base.join(link)
            }
            None => Url::parse(link),
        }
    }
}

/// Turn what was typed in the address bar into an address: a URL, or a
/// search for anything that does not look like one.
pub fn address_to_url(text: &str) -> Option<Url> {
    let text = text.trim();
    let looks_like_url = text.contains("://")
        || (!text.contains(' ') && (text.contains('.') || text.starts_with("localhost")));
    if looks_like_url {
        if let Some(u) = Url::parse(text) {
            return Some(u);
        }
    }
    Url::parse(&format!(
        "https://html.duckduckgo.com/html/?q={}",
        url::encode_query(text)
    ))
}

/// Download and parse a page. Never fails: errors become an error page.
pub fn load(url: &Url, form: Option<&String>) -> Page {
    match http::get(url, form.map(|f| f.as_str())) {
        Ok(resp) => {
            let ct = resp.content_type.clone();
            let is_text = ct.is_empty() || ct.starts_with("text/") || ct.contains("html") || ct.contains("xml");
            if !is_text {
                return message_page(
                    Some(resp.url),
                    "Этот файл не показать",
                    &format!("Сервер прислал файл типа {} ({} байт). EverBrowser показывает только веб-страницы и текст.", ct, resp.body.len()),
                );
            }
            let doc = html::parse(&resp.body, &ct);
            crate::serial::write_str("\nbrowser: loaded page\n");
            Page {
                url: Some(resp.url),
                doc,
            }
        }
        Err(e) => message_page(
            Some(url.clone()),
            "Не удаётся открыть страницу",
            &format!(
                "{}: {}. Проверьте, что QEMU запущен с сетью (-nic user,model=e1000) и у компьютера есть интернет.",
                url.host, e
            ),
        ),
    }
}

fn message_page(url: Option<Url>, title: &str, text: &str) -> Page {
    let source = format!(
        "<title>{0}</title><h1>{0}</h1><p>{1}</p><p><a href=\"about:home\">Домашняя страница</a></p>",
        title,
        escape(text)
    );
    Page {
        url,
        doc: html::parse(source.as_bytes(), "text/html"),
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The start page.
pub fn home() -> Page {
    let source = r#"<title>EverBrowser</title>
<center><h1>EverBrowser</h1>
<p>Браузер EverOS, написанный с нуля на Rust: драйвер сетевой карты e1000, TCP/IP, DNS, HTTP и HTTPS (TLS 1.3).</p>
<form action="https://html.duckduckgo.com/html/"><input type="text" name="q" placeholder="Поиск в DuckDuckGo"> <input type="submit" value="Найти"></form>
</center>
<h3>Попробуйте эти сайты</h3>
<ul>
<li><a href="http://example.com/">example.com</a> - классическая страница-пример</li>
<li><a href="http://info.cern.ch/hypertext/WWW/TheProject.html">info.cern.ch</a> - самый первый сайт в мире</li>
<li><a href="https://ru.wikipedia.org/wiki/Операционная_система">Википедия: Операционная система</a></li>
<li><a href="https://en.wikipedia.org/wiki/Operating_system">Wikipedia: Operating system</a></li>
<li><a href="http://frogfind.com/">FrogFind</a> - поисковик для старых компьютеров, упрощает любые сайты</li>
<li><a href="http://68k.news/">68k.news</a> - новости для старых браузеров</li>
<li><a href="https://lite.duckduckgo.com/lite/">DuckDuckGo Lite</a></li>
<li><a href="https://text.npr.org/">NPR</a> - новости текстом</li>
<li><a href="https://lite.cnn.com/">CNN Lite</a></li>
</ul>
<h3>Как пользоваться</h3>
<p>Введите адрес или поисковый запрос в строку сверху и нажмите Enter. Щёлкните по ссылке, чтобы перейти. Кнопка со стрелкой возвращает назад, круглая стрелка загружает страницу заново, домик открывает эту страницу. Прокрутка: колёсико мыши, полоса справа, стрелки, Page Up, Page Down и пробел. Ctrl+L переходит в адресную строку.</p>
<p><small>Пока без JavaScript, CSS и картинок. HTTPS шифрует соединение, но сертификаты сайтов не проверяются.</small></p>
"#;
    Page {
        url: None,
        doc: html::parse(source.as_bytes(), "text/html; charset=utf-8"),
    }
}
