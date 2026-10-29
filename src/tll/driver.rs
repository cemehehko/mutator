//! TLL как бэкенд фасада [`crate::io`]: узнаёт файл по meta-кадру, читает и
//! пишет его libtll, Lua — tll-lua (код как у `tll-read`, [`crate::lua`]).
//!
//! Первый кадр TLL-файла — meta: `{i32 size; i32 msgid; i64 seq}` с msgid
//! `0x6174654d` («Meta» в little-endian).
//!
//! Наружу: [`Tll`].

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use super::{log, quiet, Context, Reader, Transformer, Writer};
use crate::io::link::DriverLink;
use crate::io::{
    Driver, DriverContext, DriverReader, DriverTransformer, DriverWriter, Info, Msg, Open,
};
use crate::scheme::Scheme;

/// msgid meta-кадра TLL: «Meta».
const META: i32 = 0x6174654d;

/// Встроенный бэкенд TLL.
pub struct Tll;

impl Driver for Tll {
    fn name(&self) -> &'static str {
        "tll"
    }

    fn detect(&self, head: &[u8]) -> bool {
        head.get(4..8)
            .is_some_and(|id| i32::from_le_bytes(id.try_into().unwrap()) == META)
    }

    fn context(&self) -> Result<Box<dyn DriverContext>, String> {
        Ok(Box::new(Context::try_new()?))
    }

    fn filter_code(&self, messages: &[String], expr: &str) -> String {
        crate::lua::tll_script(messages, expr)
    }

    fn transform_template(&self) -> String {
        crate::lua::transform_template()
    }

    fn quiet(&self) {
        quiet();
    }

    fn take_error(&self) -> Option<String> {
        log::take()
    }

    fn link(&self, url: &str) -> bool {
        super::link::is_link(url)
    }

    fn link_open(&self, url: &str) -> String {
        super::link::default_open(url).into()
    }
}

impl DriverContext for Context {
    fn ensure_lua(&self) -> bool {
        Context::ensure_lua(self)
    }

    fn reader(&self, o: &Open) -> Result<Box<dyn DriverReader>, String> {
        Ok(Box::new(Reader::open(self, o)?))
    }

    fn writer(
        &self,
        path: &Path,
        scheme: &str,
        like: &Info,
        overwrite: bool,
    ) -> Result<Box<dyn DriverWriter>, String> {
        Ok(Box::new(Writer::open(self, path, scheme, like, overwrite)?))
    }

    fn transformer(
        &self,
        code: &str,
        scheme_url: &str,
        filter: bool,
    ) -> Result<Box<dyn DriverTransformer>, String> {
        let t = if filter {
            Transformer::filter(self, code, scheme_url)?
        } else {
            Transformer::new(self, code, scheme_url)?
        };
        Ok(Box::new(t))
    }

    fn link(&self, url: &str, open: &str) -> Result<Box<dyn DriverLink>, String> {
        Ok(Box::new(super::link::Link::open(self, url, open)?))
    }
}

impl DriverReader for Reader {
    fn next(&mut self) -> Option<Msg> {
        Iterator::next(self)
    }

    fn set_stop(&mut self, stop: Arc<AtomicBool>) {
        self.stop = Some(stop);
    }

    fn error(&self) -> Option<String> {
        Reader::error(self)
    }

    fn scheme(&self) -> Option<Scheme> {
        Reader::scheme(self)
    }

    fn scheme_url(&self) -> Option<String> {
        Reader::scheme_url(self)
    }

    fn info(&self) -> Info {
        Reader::info(self)
    }
}

impl DriverWriter for Writer {
    fn post(&mut self, m: &Msg) -> Result<(), String> {
        Writer::post(self, m)
    }
}

impl DriverTransformer for Transformer {
    fn transform(&mut self, m: &Msg) -> Result<Option<Msg>, String> {
        Transformer::transform(self, m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(msgid: i32) -> Vec<u8> {
        let mut v = 100u32.to_le_bytes().to_vec();
        v.extend(msgid.to_le_bytes());
        v.extend((-1i64).to_le_bytes());
        v
    }

    /// `agr:backend-choice` — TLL узнаёт свой meta-кадр и только его.
    #[test]
    fn detects_tll_meta_frame_only() {
        assert!(Tll.detect(&frame(META)));
        assert!(!Tll.detect(&frame(META + 0x2000)), "чужой meta");
        assert!(!Tll.detect(&frame(10)), "данные без meta");
        assert!(!Tll.detect(&[1, 2, 3]), "короче заголовка");
    }

    /// Живые эталоны TLL узнаются как TLL.
    #[test]
    fn tll_samples_are_detected_as_tll() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        let mut seen = 0;
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "dat") {
                let head: Vec<u8> = std::fs::read(&p).unwrap().into_iter().take(64).collect();
                assert!(Tll.detect(&head), "{p:?}");
                seen += 1;
            }
        }
        assert!(seen > 0, "эталоны на месте");
    }
}
