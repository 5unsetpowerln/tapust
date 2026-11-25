//! libadwaita's stylesheet, loaded by tapust instead of libadwaita.
//!
//! libadwaita loads its whole stylesheet again for each change of the color
//! scheme, and Ubuntu's build (Yaru accents) then parses it a second time
//! for the provider's media queries, so before the first window it parses
//! the sheet two to four times, about 23 ms each. With GTK_THEME set before
//! GTK starts, libadwaita leaves its stylesheet out, and tapust loads the
//! same sheets once the color scheme is known, parsing each once.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use gtk::{gdk, gio, pango};

/// tapust set GTK_THEME, so libadwaita has no stylesheet.
static TAKEN: AtomicBool = AtomicBool::new(false);

/// GTK_THEME names libadwaita's empty theme, which keeps GTK's own theme out
/// as libadwaita's gtk-theme-name does. Left alone when the user set one.
/// Must run before GTK starts and before any thread does.
pub fn take_over() {
    if std::env::var_os("GTK_THEME").is_none() {
        crate::pty::set_for_gtk("GTK_THEME", "Adwaita-empty");
        TAKEN.store(true, Ordering::Relaxed);
    }
}

/// The providers of adw-style-manager.c's update_stylesheet.
pub struct Stylesheet {
    display: gdk::Display,
    main: gtk::CssProvider,
    /// The sheet and media queries `main` has.
    loaded: RefCell<String>,
    accent: gtk::CssProvider,
    fonts: gtk::CssProvider,
}

impl Stylesheet {
    /// The stylesheet following `style`, or None when libadwaita has it.
    pub fn follow(display: &gdk::Display, style: &adw::StyleManager) -> Option<Rc<Stylesheet>> {
        if !TAKEN.load(Ordering::Relaxed) {
            return None;
        }
        let sheet = Rc::new(Stylesheet {
            display: display.clone(),
            main: gtk::CssProvider::new(),
            loaded: RefCell::new(String::new()),
            accent: gtk::CssProvider::new(),
            fonts: gtk::CssProvider::new(),
        });
        for provider in [&sheet.main, &sheet.accent, &sheet.fonts] {
            gtk::style_context_add_provider_for_display(display, provider, gtk::STYLE_PROVIDER_PRIORITY_THEME);
        }
        sheet.update(style);
        let weak = Rc::downgrade(&sheet);
        let update = move |style: &adw::StyleManager| {
            if let Some(sheet) = weak.upgrade() {
                sheet.update(style);
            }
        };
        style.connect_dark_notify(update.clone());
        style.connect_high_contrast_notify(update.clone());
        style.connect_accent_color_rgba_notify(update.clone());
        style.connect_document_font_name_notify(update.clone());
        style.connect_monospace_font_name_notify(update.clone());
        let (weak, manager) = (Rc::downgrade(&sheet), style.clone());
        gtk::Settings::for_display(display).connect_notify_local(Some("gtk-interface-reduced-motion"), move |_, _| {
            if let Some(sheet) = weak.upgrade() {
                sheet.update(&manager);
            }
        });
        Some(sheet)
    }

    fn update(&self, style: &adw::StyleManager) {
        let dark = style.is_dark();
        // Ubuntu's build picks a Yaru sheet with the accent in it.
        let yaru = style
            .find_property("yaru-accent")
            .and_then(|_| style.property::<Option<String>>("yaru-accent"))
            .map(|accent| format!("/org/gnome/Adwaita/styles/default-{}-yaru-{accent}.css", if dark { "dark" } else { "light" }))
            .filter(|path| gio::resources_get_info(path, gio::ResourceLookupFlags::NONE).is_ok());
        let path = yaru.clone().unwrap_or_else(|| "/org/gnome/Adwaita/styles/gtk.css".to_owned());
        let scheme = if dark { gtk::InterfaceColorScheme::Dark } else { gtk::InterfaceColorScheme::Light };
        let contrast =
            if style.is_high_contrast() { gtk::InterfaceContrast::More } else { gtk::InterfaceContrast::NoPreference };
        let motion = gtk::Settings::for_display(&self.display).property_value("gtk-interface-reduced-motion");
        let key = format!("{path} {scheme:?} {contrast:?} {motion:?}");
        if *self.loaded.borrow() != key {
            // Emptied first: setting a loaded provider's media queries parses
            // its sheet again.
            self.main.load_from_string("");
            self.main.set_prefers_color_scheme(scheme);
            self.main.set_prefers_contrast(contrast);
            self.main.set_property_from_value("prefers-reduced-motion", &motion);
            self.main.load_from_resource(&path);
            *self.loaded.borrow_mut() = key;
        }
        let accent = match yaru {
            Some(_) => String::new(),
            None => format!(
                "@define-color accent_bg_color {};\n@define-color accent_fg_color white;\n",
                style.accent_color_rgba().to_str()
            ),
        };
        self.accent.load_from_string(&accent);
        self.fonts.load_from_string(&fonts_css(&style.document_font_name(), &style.monospace_font_name()));
    }
}

/// adw-style-manager.c's generate_fonts_css.
fn fonts_css(document: &str, monospace: &str) -> String {
    let font = |name: &str, kind: &str, family: &str| {
        let desc = pango::FontDescription::from_string(name);
        let set = desc.set_fields();
        let family = match desc.family() {
            Some(f) if set.contains(pango::FontMask::FAMILY) => f.to_string(),
            _ => family.to_owned(),
        };
        let size = if set.contains(pango::FontMask::SIZE) {
            let points = f64::from(desc.size()) / f64::from(pango::SCALE);
            format!("{points}{}", if desc.is_size_absolute() { "px" } else { "pt" })
        } else {
            "10pt".to_owned()
        };
        format!("  --{kind}-font-family: {family};\n  --{kind}-font-size: {size};\n")
    };
    format!(":root {{\n{}{}}}", font(document, "document", "Sans"), font(monospace, "monospace", "Monospace"))
}

#[cfg(test)]
mod tests {
    use super::fonts_css;

    #[test]
    fn fonts_like_libadwaita() {
        assert_eq!(
            fonts_css("Adwaita Sans 11", "Monospace"),
            ":root {\n  --document-font-family: Adwaita Sans;\n  --document-font-size: 11pt;\n  \
             --monospace-font-family: Monospace;\n  --monospace-font-size: 10pt;\n}"
        );
        assert_eq!(
            fonts_css("Ubuntu 10.5", "Ubuntu Mono 13"),
            ":root {\n  --document-font-family: Ubuntu;\n  --document-font-size: 10.5pt;\n  \
             --monospace-font-family: Ubuntu Mono;\n  --monospace-font-size: 13pt;\n}"
        );
    }
}
