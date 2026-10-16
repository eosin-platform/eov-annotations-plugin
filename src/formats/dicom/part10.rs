//! A small DICOM Part 10 reader and writer (the file format of PS3.10 with
//! the encodings of PS3.5), enough for annotation objects.
//!
//! - A file is a 128-byte preamble, `DICM`, the file meta group (0002) in
//!   explicit VR little endian, then the data set.
//! - Data sets are read in explicit VR little endian (and in every transfer
//!   syntax that encodes the data set that way, such as the compressed
//!   pixel data syntaxes) or in implicit VR little endian; deflated and
//!   big-endian data sets are refused. They are written in explicit VR
//!   little endian, sequences and items with defined lengths.
//! - Sequences and their items may have defined or undefined lengths. An
//!   explicit VR `UN` element of undefined length is a sequence encoded in
//!   implicit VR (PS3.5 6.2.2); in implicit VR, any element of undefined
//!   length is a sequence, and so are the defined-length elements of
//!   [`SEQUENCES`].
//! - Values keep their bytes, padding included; the accessors interpret
//!   them (text as UTF-8 when it is valid UTF-8, else as ISO 8859-1, which
//!   covers `ISO_IR 192`, the default repertoire and `ISO_IR 100`).
//!
//! Hostile input never panics and never allocates more than the file
//! holds: every length is checked against the bytes that remain in the
//! enclosing value, nesting is limited to [`MAX_DEPTH`] levels and a file
//! to [`MAX_ELEMENTS`] elements and items.

use std::collections::BTreeMap;
use std::fmt;

/// Deepest nesting of sequences and items (a sequence and its item are two
/// levels).
pub const MAX_DEPTH: usize = 32;
/// Most elements and items read from one file.
pub const MAX_ELEMENTS: usize = 1_000_000;

/// The value length that means "undefined" (delimited).
const UNDEFINED: u32 = 0xFFFF_FFFF;

pub const IMPLICIT_VR_LITTLE_ENDIAN: &str = "1.2.840.10008.1.2";
pub const EXPLICIT_VR_LITTLE_ENDIAN: &str = "1.2.840.10008.1.2.1";
const DEFLATED_EXPLICIT_VR_LITTLE_ENDIAN: &str = "1.2.840.10008.1.2.1.99";
const EXPLICIT_VR_BIG_ENDIAN: &str = "1.2.840.10008.1.2.2";

/// A data element tag: (group, element).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tag(pub u16, pub u16);

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({:04X},{:04X})", self.0, self.1)
    }
}

const ITEM: Tag = Tag(0xFFFE, 0xE000);
const ITEM_DELIMITATION: Tag = Tag(0xFFFE, 0xE00D);
const SEQUENCE_DELIMITATION: Tag = Tag(0xFFFE, 0xE0DD);

pub const FILE_META_INFORMATION_GROUP_LENGTH: Tag = Tag(0x0002, 0x0000);
pub const FILE_META_INFORMATION_VERSION: Tag = Tag(0x0002, 0x0001);
pub const MEDIA_STORAGE_SOP_CLASS_UID: Tag = Tag(0x0002, 0x0002);
pub const MEDIA_STORAGE_SOP_INSTANCE_UID: Tag = Tag(0x0002, 0x0003);
pub const TRANSFER_SYNTAX_UID: Tag = Tag(0x0002, 0x0010);
pub const IMPLEMENTATION_CLASS_UID: Tag = Tag(0x0002, 0x0012);
pub const IMPLEMENTATION_VERSION_NAME: Tag = Tag(0x0002, 0x0013);

/// The sequences an implicit VR data set is read into (implicit VR has no
/// VRs, so these are known by their tags): the ones of annotation objects
/// and the common ones around them.
pub const SEQUENCES: &[Tag] = &[
    Tag(0x0008, 0x0110), // Coding Scheme Identification Sequence
    Tag(0x0008, 0x1032), // Procedure Code Sequence
    Tag(0x0008, 0x1111), // Referenced Performed Procedure Step Sequence
    Tag(0x0008, 0x1115), // Referenced Series Sequence
    Tag(0x0008, 0x1140), // Referenced Image Sequence
    Tag(0x0008, 0x114A), // Referenced Instance Sequence
    Tag(0x0008, 0x1200), // Studies Containing Other Referenced Instances Sequence
    Tag(0x0008, 0x2218), // Anatomic Region Sequence
    Tag(0x0008, 0x2220), // Anatomic Region Modifier Sequence
    Tag(0x0008, 0x2228), // Primary Anatomic Structure Sequence
    Tag(0x0008, 0x2230), // Primary Anatomic Structure Modifier Sequence
    Tag(0x0010, 0x0024), // Issuer of Patient ID Qualifiers Sequence
    Tag(0x0010, 0x1002), // Other Patient IDs Sequence
    Tag(0x0040, 0x0275), // Request Attributes Sequence
    Tag(0x0040, 0x0513), // Issuer of the Container Identifier Sequence
    Tag(0x0040, 0x0518), // Container Type Code Sequence
    Tag(0x0040, 0x0560), // Specimen Description Sequence
    Tag(0x0040, 0x08EA), // Measurement Units Code Sequence
    Tag(0x0040, 0xA043), // Concept Name Code Sequence
    Tag(0x0040, 0xA168), // Concept Code Sequence
    Tag(0x0040, 0xA730), // Content Sequence
    Tag(0x0066, 0x002F), // Algorithm Family Code Sequence
    Tag(0x0066, 0x0030), // Algorithm Name Code Sequence
    Tag(0x0066, 0x0121), // Measurements Sequence
    Tag(0x0066, 0x0132), // Measurement Values Sequence
    Tag(0x006A, 0x0002), // Annotation Group Sequence
    Tag(0x006A, 0x0008), // Annotation Group Algorithm Identification Sequence
    Tag(0x006A, 0x0009), // Annotation Property Category Code Sequence
    Tag(0x006A, 0x000A), // Annotation Property Type Code Sequence
    Tag(0x006A, 0x000B), // Annotation Property Type Modifier Code Sequence
    Tag(0x0070, 0x0086), // Content Creator's Identification Code Sequence
    Tag(0x0070, 0x0087), // Alternate Content Description Sequence
    Tag(0x0088, 0x0200), // Icon Image Sequence
];

/// A value representation: two upper-case letters.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Vr(pub [u8; 2]);

impl fmt::Debug for Vr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl fmt::Display for Vr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            if b.is_ascii_graphic() {
                write!(f, "{}", b as char)?;
            } else {
                write!(f, "\\x{b:02x}")?;
            }
        }
        Ok(())
    }
}

impl Vr {
    pub const AE: Vr = Vr(*b"AE");
    pub const AS: Vr = Vr(*b"AS");
    pub const AT: Vr = Vr(*b"AT");
    pub const CS: Vr = Vr(*b"CS");
    pub const DA: Vr = Vr(*b"DA");
    pub const DS: Vr = Vr(*b"DS");
    pub const DT: Vr = Vr(*b"DT");
    pub const FD: Vr = Vr(*b"FD");
    pub const FL: Vr = Vr(*b"FL");
    pub const IS: Vr = Vr(*b"IS");
    pub const LO: Vr = Vr(*b"LO");
    pub const LT: Vr = Vr(*b"LT");
    pub const OB: Vr = Vr(*b"OB");
    pub const OD: Vr = Vr(*b"OD");
    pub const OF: Vr = Vr(*b"OF");
    pub const OL: Vr = Vr(*b"OL");
    pub const OV: Vr = Vr(*b"OV");
    pub const OW: Vr = Vr(*b"OW");
    pub const PN: Vr = Vr(*b"PN");
    pub const SH: Vr = Vr(*b"SH");
    pub const SL: Vr = Vr(*b"SL");
    pub const SQ: Vr = Vr(*b"SQ");
    pub const SS: Vr = Vr(*b"SS");
    pub const ST: Vr = Vr(*b"ST");
    pub const SV: Vr = Vr(*b"SV");
    pub const TM: Vr = Vr(*b"TM");
    pub const UC: Vr = Vr(*b"UC");
    pub const UI: Vr = Vr(*b"UI");
    pub const UL: Vr = Vr(*b"UL");
    pub const UN: Vr = Vr(*b"UN");
    pub const UR: Vr = Vr(*b"UR");
    pub const US: Vr = Vr(*b"US");
    pub const UT: Vr = Vr(*b"UT");
    pub const UV: Vr = Vr(*b"UV");

    pub fn is_known(self) -> bool {
        listed(ALL_VRS, self)
    }

    /// Explicit VR encodes these with two reserved bytes and a 32-bit
    /// length; the others with a 16-bit length.
    fn has_long_length(self) -> bool {
        listed(b"OBODOFOLOVOWSQSVUCUNURUTUV", self)
    }

    /// Character strings, padded with spaces.
    fn is_text(self) -> bool {
        listed(b"AEASCSDADSDTISLOLTPNSHSTTMUCURUT", self)
    }

    /// Texts whose leading spaces are significant (the others are trimmed
    /// on both sides) and that have a single value (no `\` separator).
    fn is_free_text(self) -> bool {
        listed(b"LTSTUTUR", self)
    }

    /// The byte that pads a value to an even length.
    fn padding(self) -> u8 {
        if self.is_text() { b' ' } else { 0 }
    }
}

/// Every value representation of PS3.5, two letters each.
const ALL_VRS: &[u8] = b"AEASATCSDADSDTFDFLISLOLTOBODOFOLOVOWPNSHSLSQSSSTSVTMUCUIULUNURUSUTUV";

/// Whether `vr` is one of the two-letter codes in `list`.
fn listed(list: &[u8], vr: Vr) -> bool {
    list.as_chunks::<2>().0.contains(&vr.0)
}

/// An element value: its bytes (little endian, as stored) or, for a
/// sequence, its items.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bytes(Vec<u8>),
    Items(Vec<DataSet>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    pub vr: Vr,
    pub value: Value,
}

/// A data set: elements by tag, in ascending tag order (the order they are
/// written in).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DataSet {
    elements: BTreeMap<Tag, Element>,
}

/// Text as stored: UTF-8 when valid, else ISO 8859-1.
fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
    }
}

/// Fixed-size little-endian words of a binary value, or `None` when its
/// length is not a multiple of the word size.
fn words<const N: usize>(bytes: &[u8]) -> Option<impl Iterator<Item = [u8; N]> + '_> {
    let (words, rest) = bytes.as_chunks::<N>();
    rest.is_empty().then(|| words.iter().copied())
}

impl DataSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.elements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Tag, &Element)> {
        self.elements.iter()
    }

    pub fn get(&self, tag: Tag) -> Option<&Element> {
        self.elements.get(&tag)
    }

    pub fn contains(&self, tag: Tag) -> bool {
        self.elements.contains_key(&tag)
    }

    pub fn remove(&mut self, tag: Tag) -> Option<Element> {
        self.elements.remove(&tag)
    }

    // -- writing --------------------------------------------------------------

    pub fn insert(&mut self, tag: Tag, vr: Vr, value: Value) {
        self.elements.insert(tag, Element { vr, value });
    }

    pub fn put_bytes(&mut self, tag: Tag, vr: Vr, bytes: Vec<u8>) {
        self.insert(tag, vr, Value::Bytes(bytes));
    }

    /// A text value (UTF-8; written padded to an even length).
    pub fn put_text(&mut self, tag: Tag, vr: Vr, text: &str) {
        self.put_bytes(tag, vr, text.as_bytes().to_vec());
    }

    pub fn put_u16s(&mut self, tag: Tag, values: &[u16]) {
        let bytes = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.put_bytes(tag, Vr::US, bytes);
    }

    pub fn put_u32(&mut self, tag: Tag, value: u32) {
        self.put_bytes(tag, Vr::UL, value.to_le_bytes().to_vec());
    }

    /// 32-bit unsigned words (`OL`, `UL`).
    pub fn put_u32s(&mut self, tag: Tag, vr: Vr, values: &[u32]) {
        let bytes = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.put_bytes(tag, vr, bytes);
    }

    /// 32-bit floats (`OF`, `FL`).
    pub fn put_f32s(&mut self, tag: Tag, vr: Vr, values: &[f32]) {
        let bytes = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.put_bytes(tag, vr, bytes);
    }

    pub fn put_items(&mut self, tag: Tag, items: Vec<DataSet>) {
        self.insert(tag, Vr::SQ, Value::Items(items));
    }

    // -- reading --------------------------------------------------------------

    /// The bytes of a value (`None` for sequences and missing elements).
    pub fn bytes(&self, tag: Tag) -> Option<&[u8]> {
        match &self.get(tag)?.value {
            Value::Bytes(bytes) => Some(bytes),
            Value::Items(_) => None,
        }
    }

    /// A text value, without its padding and surrounding spaces (only
    /// trailing ones for LT, ST, UT and UR); multiple values stay joined by
    /// `\`.
    pub fn text(&self, tag: Tag) -> Option<String> {
        let element = self.get(tag)?;
        let Value::Bytes(bytes) = &element.value else {
            return None;
        };
        let text = decode(bytes);
        let text = text.trim_end_matches(['\0', ' ']);
        Some(if element.vr.is_free_text() {
            text.to_string()
        } else {
            text.trim_start_matches(' ').to_string()
        })
    }

    /// The values of a multi-valued text, each trimmed.
    pub fn texts(&self, tag: Tag) -> Vec<String> {
        let Some(text) = self.text(tag) else {
            return Vec::new();
        };
        if text.is_empty() {
            return Vec::new();
        }
        text.split('\\')
            .map(|v| v.trim_matches(['\0', ' ']).to_string())
            .collect()
    }

    /// An unsigned integer: a `US` or `UL` value (its first word), or a
    /// number written as text (`IS`).
    pub fn uint(&self, tag: Tag) -> Option<u64> {
        let element = self.get(tag)?;
        let Value::Bytes(bytes) = &element.value else {
            return None;
        };
        if element.vr.is_text() {
            return self.text(tag)?.split('\\').next()?.trim().parse().ok();
        }
        match bytes.len() {
            2 => Some(u64::from(u16::from_le_bytes([bytes[0], bytes[1]]))),
            n if n >= 4 && element.vr != Vr::US => Some(u64::from(u32::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3],
            ]))),
            n if n >= 2 => Some(u64::from(u16::from_le_bytes([bytes[0], bytes[1]]))),
            _ => None,
        }
    }

    /// 16-bit unsigned words (`US`), or `None` when missing or of an odd
    /// length.
    pub fn u16s(&self, tag: Tag) -> Option<Vec<u16>> {
        Some(
            words::<2>(self.bytes(tag)?)?
                .map(u16::from_le_bytes)
                .collect(),
        )
    }

    /// 32-bit unsigned words (`OL`, `UL`); `Err` when the length is not a
    /// multiple of 4.
    pub fn u32s(&self, tag: Tag) -> Result<Option<Vec<u32>>, String> {
        let Some(bytes) = self.bytes(tag) else {
            return Ok(None);
        };
        words::<4>(bytes)
            .map(|w| Some(w.map(u32::from_le_bytes).collect()))
            .ok_or_else(|| format!("{tag} is {} bytes long, not 32-bit words", bytes.len()))
    }

    /// 64-bit unsigned words (`OV`, `UV`).
    pub fn u64s(&self, tag: Tag) -> Result<Option<Vec<u64>>, String> {
        let Some(bytes) = self.bytes(tag) else {
            return Ok(None);
        };
        words::<8>(bytes)
            .map(|w| Some(w.map(u64::from_le_bytes).collect()))
            .ok_or_else(|| format!("{tag} is {} bytes long, not 64-bit words", bytes.len()))
    }

    /// 32-bit floats (`OF`, `FL`).
    pub fn f32s(&self, tag: Tag) -> Result<Option<Vec<f32>>, String> {
        let Some(bytes) = self.bytes(tag) else {
            return Ok(None);
        };
        words::<4>(bytes)
            .map(|w| Some(w.map(f32::from_le_bytes).collect()))
            .ok_or_else(|| format!("{tag} is {} bytes long, not 32-bit floats", bytes.len()))
    }

    /// 64-bit floats (`OD`, `FD`).
    pub fn f64s(&self, tag: Tag) -> Result<Option<Vec<f64>>, String> {
        let Some(bytes) = self.bytes(tag) else {
            return Ok(None);
        };
        words::<8>(bytes)
            .map(|w| Some(w.map(f64::from_le_bytes).collect()))
            .ok_or_else(|| format!("{tag} is {} bytes long, not 64-bit floats", bytes.len()))
    }

    /// The items of a sequence (none when missing or not a sequence).
    pub fn items(&self, tag: Tag) -> &[DataSet] {
        match self.get(tag).map(|e| &e.value) {
            Some(Value::Items(items)) => items,
            _ => &[],
        }
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// How a data set is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    ExplicitLittle,
    ImplicitLittle,
}

/// A parsed file.
#[derive(Debug, Clone, PartialEq)]
pub struct FileObject {
    pub meta: DataSet,
    pub dataset: DataSet,
    pub syntax: Syntax,
}

/// Whether `bytes` start like a DICOM Part 10 file.
pub fn is_part10(bytes: &[u8]) -> bool {
    bytes.get(128..132) == Some(b"DICM".as_slice())
}

/// A window on the bytes of a file (or of one value in it).
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
    /// Where `bytes` start in the file, for messages.
    base: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8], base: usize) -> Self {
        Self {
            bytes,
            pos: 0,
            base,
        }
    }

    fn offset(&self) -> usize {
        self.base + self.pos
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if n > self.remaining() {
            return Err(format!(
                "it ends at byte {} in the middle of a value",
                self.base + self.bytes.len()
            ));
        }
        let slice = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn tag(&mut self) -> Result<Tag, String> {
        Ok(Tag(self.u16()?, self.u16()?))
    }

    fn peek_tag(&self) -> Option<Tag> {
        let b = self.bytes.get(self.pos..self.pos + 4)?;
        Some(Tag(
            u16::from_le_bytes([b[0], b[1]]),
            u16::from_le_bytes([b[2], b[3]]),
        ))
    }
}

enum Next {
    Element(Tag, Element),
    /// An item delimitation item.
    ItemEnd,
}

#[derive(Default)]
struct Parser {
    elements: usize,
}

impl Parser {
    fn count(&mut self) -> Result<(), String> {
        self.elements += 1;
        if self.elements > MAX_ELEMENTS {
            return Err(format!("it has more than {MAX_ELEMENTS} elements"));
        }
        Ok(())
    }

    /// The next element of a data set.
    fn next(&mut self, cur: &mut Cursor<'_>, syntax: Syntax, depth: usize) -> Result<Next, String> {
        let start = cur.offset();
        let tag = cur.tag()?;
        if tag == ITEM_DELIMITATION {
            cur.u32()?;
            return Ok(Next::ItemEnd);
        }
        if tag.0 == 0xFFFE {
            return Err(format!("{tag} at byte {start} is out of place"));
        }
        let (vr, length) = match syntax {
            Syntax::ExplicitLittle => {
                let code = cur.take(2)?;
                let vr = Vr([code[0], code[1]]);
                if !vr.is_known() {
                    return Err(format!(
                        "element {tag} at byte {start} has an unknown value representation \"{vr}\""
                    ));
                }
                let length = if vr.has_long_length() {
                    cur.take(2)?;
                    cur.u32()?
                } else {
                    u32::from(cur.u16()?)
                };
                (vr, length)
            }
            Syntax::ImplicitLittle => {
                let vr = if SEQUENCES.contains(&tag) {
                    Vr::SQ
                } else {
                    Vr::UN
                };
                (vr, cur.u32()?)
            }
        };
        self.count()?;
        let value = if length == UNDEFINED {
            match (syntax, vr) {
                (_, Vr::SQ) => Value::Items(self.sequence(cur, syntax, true, depth + 1)?),
                (Syntax::ExplicitLittle, Vr::UN) | (Syntax::ImplicitLittle, _) => {
                    Value::Items(self.sequence(cur, Syntax::ImplicitLittle, true, depth + 1)?)
                }
                // Encapsulated (compressed) pixel data: fragments that
                // nothing here needs.
                (_, Vr::OB | Vr::OW) => {
                    self.fragments(cur)?;
                    Value::Bytes(Vec::new())
                }
                _ => {
                    return Err(format!(
                        "element {tag} at byte {start} ({vr}) has an undefined length"
                    ));
                }
            }
        } else {
            let length = length as usize;
            if length > cur.remaining() {
                return Err(format!(
                    "element {tag} at byte {start} is {length} bytes long, but only {} remain",
                    cur.remaining()
                ));
            }
            let base = cur.offset();
            let body = cur.take(length)?;
            if vr == Vr::SQ {
                let mut inner = Cursor::new(body, base);
                Value::Items(self.sequence(&mut inner, syntax, false, depth + 1)?)
            } else {
                Value::Bytes(body.to_vec())
            }
        };
        Ok(Next::Element(tag, Element { vr, value }))
    }

    /// Elements up to the end of `cur` (`delimited`: up to an item
    /// delimitation item).
    fn dataset(
        &mut self,
        cur: &mut Cursor<'_>,
        syntax: Syntax,
        delimited: bool,
        depth: usize,
    ) -> Result<DataSet, String> {
        if depth > MAX_DEPTH {
            return Err(format!(
                "its sequences are nested more than {MAX_DEPTH} levels deep"
            ));
        }
        let mut dataset = DataSet::default();
        loop {
            if cur.at_end() {
                if delimited {
                    return Err(format!("it ends at byte {} inside an item", cur.offset()));
                }
                return Ok(dataset);
            }
            let start = cur.offset();
            match self.next(cur, syntax, depth)? {
                Next::ItemEnd if delimited => return Ok(dataset),
                Next::ItemEnd => {
                    return Err(format!("an item delimiter at byte {start} is out of place"));
                }
                // Group lengths say nothing the elements do not.
                Next::Element(tag, _) if tag.1 == 0x0000 => {}
                // The first of duplicated elements wins.
                Next::Element(tag, element) => {
                    dataset.elements.entry(tag).or_insert(element);
                }
            }
        }
    }

    /// The items of a sequence (`delimited`: up to a sequence delimitation
    /// item, else up to the end of `cur`).
    fn sequence(
        &mut self,
        cur: &mut Cursor<'_>,
        syntax: Syntax,
        delimited: bool,
        depth: usize,
    ) -> Result<Vec<DataSet>, String> {
        if depth > MAX_DEPTH {
            return Err(format!(
                "its sequences are nested more than {MAX_DEPTH} levels deep"
            ));
        }
        let mut items = Vec::new();
        loop {
            if cur.at_end() {
                if delimited {
                    return Err(format!(
                        "it ends at byte {} inside a sequence",
                        cur.offset()
                    ));
                }
                return Ok(items);
            }
            let start = cur.offset();
            let tag = cur.tag()?;
            let length = cur.u32()?;
            match tag {
                SEQUENCE_DELIMITATION if delimited => return Ok(items),
                ITEM => {
                    self.count()?;
                    if length == UNDEFINED {
                        items.push(self.dataset(cur, syntax, true, depth + 1)?);
                    } else {
                        let length = length as usize;
                        if length > cur.remaining() {
                            return Err(format!(
                                "the item at byte {start} is {length} bytes long, but only {} remain",
                                cur.remaining()
                            ));
                        }
                        let base = cur.offset();
                        let mut inner = Cursor::new(cur.take(length)?, base);
                        items.push(self.dataset(&mut inner, syntax, false, depth + 1)?);
                    }
                }
                other => {
                    return Err(format!(
                        "a sequence item was expected at byte {start}, not {other}"
                    ));
                }
            }
        }
    }

    /// Skips the fragments of encapsulated pixel data.
    fn fragments(&mut self, cur: &mut Cursor<'_>) -> Result<(), String> {
        loop {
            let start = cur.offset();
            let tag = cur.tag()?;
            let length = cur.u32()?;
            match tag {
                SEQUENCE_DELIMITATION => return Ok(()),
                ITEM if length != UNDEFINED => {
                    self.count()?;
                    if length as usize > cur.remaining() {
                        return Err(format!(
                            "the fragment at byte {start} is {length} bytes long, but only {} remain",
                            cur.remaining()
                        ));
                    }
                    cur.take(length as usize)?;
                }
                other => {
                    return Err(format!(
                        "a pixel data fragment was expected at byte {start}, not {other}"
                    ));
                }
            }
        }
    }
}

/// Whether the data set at `cur` looks explicit VR (a known VR after the
/// first tag), for files whose meta group names no transfer syntax.
fn sniff(cur: &Cursor<'_>) -> Syntax {
    match cur.bytes.get(cur.pos + 4..cur.pos + 6) {
        Some(code) if Vr([code[0], code[1]]).is_known() => Syntax::ExplicitLittle,
        _ => Syntax::ImplicitLittle,
    }
}

/// Reads a Part 10 file. Errors say what is wrong and where.
pub fn read(bytes: &[u8]) -> Result<FileObject, String> {
    if !is_part10(bytes) {
        return Err("it is not a DICOM file (\"DICM\" is missing at byte 128)".to_string());
    }
    let mut cur = Cursor::new(&bytes[132..], 132);
    let mut parser = Parser::default();
    let mut meta = DataSet::default();
    while cur.peek_tag().is_some_and(|t| t.0 == 0x0002) {
        let start = cur.offset();
        match parser.next(&mut cur, Syntax::ExplicitLittle, 0)? {
            // The group length is not needed to find the group's end.
            Next::Element(tag, _) if tag.1 == 0x0000 => {}
            Next::Element(tag, element) => {
                meta.elements.entry(tag).or_insert(element);
            }
            Next::ItemEnd => {
                return Err(format!("an item delimiter at byte {start} is out of place"));
            }
        }
    }
    if meta.is_empty() {
        return Err("its file meta information (group 0002) is missing".to_string());
    }
    let transfer_syntax = meta.text(TRANSFER_SYNTAX_UID).unwrap_or_default();
    let syntax = match transfer_syntax.as_str() {
        IMPLICIT_VR_LITTLE_ENDIAN => Syntax::ImplicitLittle,
        EXPLICIT_VR_BIG_ENDIAN => {
            return Err(
                "it is encoded in explicit VR big endian, a retired transfer syntax that is not supported"
                    .to_string(),
            );
        }
        DEFLATED_EXPLICIT_VR_LITTLE_ENDIAN => {
            return Err("it is deflated (compressed), which is not supported".to_string());
        }
        "" => sniff(&cur),
        // Every other transfer syntax encodes the data set in explicit VR
        // little endian (they differ in how pixel data is compressed).
        _ => Syntax::ExplicitLittle,
    };
    let dataset = parser.dataset(&mut cur, syntax, false, 0)?;
    Ok(FileObject {
        meta,
        dataset,
        syntax,
    })
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// What the file meta group says about the object.
pub struct FileMeta<'a> {
    pub sop_class_uid: &'a str,
    pub sop_instance_uid: &'a str,
    pub implementation_class_uid: &'a str,
    pub implementation_version_name: &'a str,
}

fn encode_element(tag: Tag, element: &Element, out: &mut Vec<u8>) -> Result<(), String> {
    out.extend_from_slice(&tag.0.to_le_bytes());
    out.extend_from_slice(&tag.1.to_le_bytes());
    match &element.value {
        Value::Bytes(bytes) => {
            let vr = element.vr;
            let padded = bytes.len() + bytes.len() % 2;
            out.extend_from_slice(&vr.0);
            if vr.has_long_length() {
                let length = u32::try_from(padded)
                    .ok()
                    .filter(|l| *l != UNDEFINED)
                    .ok_or_else(|| format!("{tag} is too long for a DICOM element"))?;
                out.extend_from_slice(&[0, 0]);
                out.extend_from_slice(&length.to_le_bytes());
            } else {
                let length = u16::try_from(padded)
                    .map_err(|_| format!("{tag} ({vr}) is longer than 65 534 bytes"))?;
                out.extend_from_slice(&length.to_le_bytes());
            }
            out.extend_from_slice(bytes);
            if bytes.len() % 2 == 1 {
                out.push(vr.padding());
            }
        }
        Value::Items(items) => {
            let mut body = Vec::new();
            for item in items {
                let mut encoded = Vec::new();
                encode_dataset(item, &mut encoded)?;
                let length = u32::try_from(encoded.len())
                    .ok()
                    .filter(|l| *l != UNDEFINED)
                    .ok_or_else(|| format!("an item of {tag} is too long"))?;
                body.extend_from_slice(&ITEM.0.to_le_bytes());
                body.extend_from_slice(&ITEM.1.to_le_bytes());
                body.extend_from_slice(&length.to_le_bytes());
                body.extend_from_slice(&encoded);
            }
            let length = u32::try_from(body.len())
                .ok()
                .filter(|l| *l != UNDEFINED)
                .ok_or_else(|| format!("{tag} is too long for a DICOM element"))?;
            out.extend_from_slice(&Vr::SQ.0);
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(&length.to_le_bytes());
            out.extend_from_slice(&body);
        }
    }
    Ok(())
}

fn encode_dataset(dataset: &DataSet, out: &mut Vec<u8>) -> Result<(), String> {
    for (tag, element) in &dataset.elements {
        encode_element(*tag, element, out)?;
    }
    Ok(())
}

/// A Part 10 file: preamble, `DICM`, the file meta group and `dataset` in
/// explicit VR little endian.
pub fn write(meta: &FileMeta<'_>, dataset: &DataSet) -> Result<Vec<u8>, String> {
    let mut group = DataSet::default();
    group.put_bytes(FILE_META_INFORMATION_VERSION, Vr::OB, vec![0x00, 0x01]);
    group.put_text(MEDIA_STORAGE_SOP_CLASS_UID, Vr::UI, meta.sop_class_uid);
    group.put_text(
        MEDIA_STORAGE_SOP_INSTANCE_UID,
        Vr::UI,
        meta.sop_instance_uid,
    );
    group.put_text(TRANSFER_SYNTAX_UID, Vr::UI, EXPLICIT_VR_LITTLE_ENDIAN);
    group.put_text(
        IMPLEMENTATION_CLASS_UID,
        Vr::UI,
        meta.implementation_class_uid,
    );
    group.put_text(
        IMPLEMENTATION_VERSION_NAME,
        Vr::SH,
        meta.implementation_version_name,
    );
    let mut body = Vec::new();
    encode_dataset(&group, &mut body)?;
    let mut out = vec![0u8; 128];
    out.extend_from_slice(b"DICM");
    let length = u32::try_from(body.len()).map_err(|_| "the file meta group is too long")?;
    encode_element(
        FILE_META_INFORMATION_GROUP_LENGTH,
        &Element {
            vr: Vr::UL,
            value: Value::Bytes(length.to_le_bytes().to_vec()),
        },
        &mut out,
    )?;
    out.extend_from_slice(&body);
    encode_dataset(dataset, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const META: FileMeta<'static> = FileMeta {
        sop_class_uid: "1.2.3",
        sop_instance_uid: "1.2.3.4",
        implementation_class_uid: "2.25.1",
        implementation_version_name: "TEST",
    };

    fn sample() -> DataSet {
        let mut code = DataSet::new();
        code.put_text(Tag(0x0008, 0x0100), Vr::SH, "91723000");
        code.put_text(Tag(0x0008, 0x0104), Vr::LO, "Anatomical Structure");
        let mut item = DataSet::new();
        item.put_items(Tag(0x006A, 0x0009), vec![code]);
        item.put_f32s(Tag(0x0066, 0x0016), Vr::OF, &[0.5, 1.5, 2.25, 3.0]);
        item.put_u32s(Tag(0x0066, 0x0040), Vr::OL, &[1, 3]);
        item.put_u16s(Tag(0x0040, 0xA180), &[7]);
        item.put_text(Tag(0x006A, 0x0005), Vr::LO, "Tumour, ünïcode");
        let mut ds = DataSet::new();
        ds.put_text(Tag(0x0008, 0x0016), Vr::UI, "1.2.3");
        ds.put_text(Tag(0x0010, 0x0010), Vr::PN, "");
        ds.put_text(Tag(0x0008, 0x0005), Vr::CS, "ISO_IR 192");
        ds.put_items(Tag(0x006A, 0x0002), vec![item.clone(), item]);
        ds.put_text(Tag(0x0077, 0x1001), Vr::UT, " leading space kept");
        ds
    }

    #[test]
    fn files_round_trip() {
        let ds = sample();
        let bytes = write(&META, &ds).unwrap();
        assert!(is_part10(&bytes));
        assert_eq!(bytes.len() % 2, 0);
        let file = read(&bytes).unwrap();
        assert_eq!(file.syntax, Syntax::ExplicitLittle);
        assert_eq!(
            file.meta.text(TRANSFER_SYNTAX_UID).as_deref(),
            Some(EXPLICIT_VR_LITTLE_ENDIAN)
        );
        assert_eq!(
            file.meta.text(MEDIA_STORAGE_SOP_INSTANCE_UID).as_deref(),
            Some("1.2.3.4")
        );
        // The meta group length is not kept; everything else is.
        assert!(!file.meta.contains(FILE_META_INFORMATION_GROUP_LENGTH));
        let groups = file.dataset.items(Tag(0x006A, 0x0002));
        assert_eq!(groups.len(), 2);
        let g = &groups[0];
        assert_eq!(
            g.f32s(Tag(0x0066, 0x0016)).unwrap().unwrap(),
            vec![0.5, 1.5, 2.25, 3.0]
        );
        assert_eq!(g.u32s(Tag(0x0066, 0x0040)).unwrap().unwrap(), vec![1, 3]);
        assert_eq!(g.uint(Tag(0x0040, 0xA180)), Some(7));
        assert_eq!(
            g.text(Tag(0x006A, 0x0005)).as_deref(),
            Some("Tumour, ünïcode")
        );
        assert_eq!(
            g.items(Tag(0x006A, 0x0009))[0]
                .text(Tag(0x0008, 0x0104))
                .as_deref(),
            Some("Anatomical Structure")
        );
        assert_eq!(file.dataset.text(Tag(0x0010, 0x0010)).as_deref(), Some(""));
        assert_eq!(
            file.dataset.text(Tag(0x0077, 0x1001)).as_deref(),
            Some(" leading space kept")
        );
        // Values are padded to even lengths: "1.2.3" with a NUL, the UT
        // text with a space.
        assert_eq!(
            file.dataset.bytes(Tag(0x0008, 0x0016)),
            Some(b"1.2.3\0".as_slice())
        );
    }

    #[test]
    fn elements_are_written_in_ascending_tag_order() {
        let bytes = write(&META, &sample()).unwrap();
        let mut cur = Cursor::new(&bytes[132..], 132);
        let mut parser = Parser::default();
        let mut last = Tag(0, 0);
        while !cur.at_end() {
            let Next::Element(tag, _) = parser.next(&mut cur, Syntax::ExplicitLittle, 0).unwrap()
            else {
                panic!()
            };
            assert!(tag > last, "{tag} after {last}");
            last = tag;
        }
    }

    /// An implicit VR data set with undefined-length sequences and items,
    /// assembled by hand.
    fn implicit_file() -> Vec<u8> {
        fn element(out: &mut Vec<u8>, tag: Tag, value: &[u8]) {
            out.extend_from_slice(&tag.0.to_le_bytes());
            out.extend_from_slice(&tag.1.to_le_bytes());
            out.extend_from_slice(&(value.len() as u32).to_le_bytes());
            out.extend_from_slice(value);
        }
        fn marker(out: &mut Vec<u8>, tag: Tag, length: u32) {
            out.extend_from_slice(&tag.0.to_le_bytes());
            out.extend_from_slice(&tag.1.to_le_bytes());
            out.extend_from_slice(&length.to_le_bytes());
        }
        let mut meta = DataSet::new();
        meta.put_text(TRANSFER_SYNTAX_UID, Vr::UI, IMPLICIT_VR_LITTLE_ENDIAN);
        let mut out = vec![0u8; 128];
        out.extend_from_slice(b"DICM");
        encode_dataset(&meta, &mut out).unwrap();
        element(&mut out, Tag(0x0008, 0x0016), b"1.2.3\0");
        // A sequence of undefined length with an undefined-length item that
        // holds a nested defined-length sequence.
        marker(&mut out, Tag(0x006A, 0x0002), UNDEFINED);
        marker(&mut out, ITEM, UNDEFINED);
        element(&mut out, Tag(0x006A, 0x0005), b"Nuclei");
        let mut code = Vec::new();
        element(&mut code, Tag(0x0008, 0x0100), b"84640000");
        let mut nested = Vec::new();
        marker(&mut nested, ITEM, code.len() as u32);
        nested.extend_from_slice(&code);
        element(&mut out, Tag(0x006A, 0x000A), &nested);
        marker(&mut out, ITEM_DELIMITATION, 0);
        marker(&mut out, SEQUENCE_DELIMITATION, 0);
        // An unknown element of undefined length is a sequence too.
        marker(&mut out, Tag(0x0099, 0x1000), UNDEFINED);
        marker(&mut out, SEQUENCE_DELIMITATION, 0);
        element(&mut out, Tag(0x0010, 0x0010), b"Anon");
        out
    }

    #[test]
    fn implicit_vr_data_sets_are_read() {
        let file = read(&implicit_file()).unwrap();
        assert_eq!(file.syntax, Syntax::ImplicitLittle);
        let ds = &file.dataset;
        assert_eq!(ds.text(Tag(0x0008, 0x0016)).as_deref(), Some("1.2.3"));
        let group = &ds.items(Tag(0x006A, 0x0002))[0];
        assert_eq!(group.text(Tag(0x006A, 0x0005)).as_deref(), Some("Nuclei"));
        assert_eq!(
            group.items(Tag(0x006A, 0x000A))[0]
                .text(Tag(0x0008, 0x0100))
                .as_deref(),
            Some("84640000")
        );
        assert!(ds.items(Tag(0x0099, 0x1000)).is_empty());
        assert_eq!(ds.text(Tag(0x0010, 0x0010)).as_deref(), Some("Anon"));
        // Without a transfer syntax, the encoding is recognised.
        let mut bytes = implicit_file();
        let file = {
            let mut meta = DataSet::new();
            meta.put_text(IMPLEMENTATION_VERSION_NAME, Vr::SH, "X");
            let mut head = vec![0u8; 128];
            head.extend_from_slice(b"DICM");
            encode_dataset(&meta, &mut head).unwrap();
            let start = 132 + 8 + IMPLICIT_VR_LITTLE_ENDIAN.len() + 1;
            head.extend_from_slice(&bytes.split_off(start));
            read(&head).unwrap()
        };
        assert_eq!(file.syntax, Syntax::ImplicitLittle);
        assert_eq!(
            file.dataset.text(Tag(0x0010, 0x0010)).as_deref(),
            Some("Anon")
        );
    }

    #[test]
    fn undefined_length_un_elements_are_implicit_vr_sequences() {
        let mut inner = Vec::new();
        // (0008,0100) in implicit VR inside an explicit VR file.
        inner.extend_from_slice(&[0x08, 0x00, 0x00, 0x01, 4, 0, 0, 0]);
        inner.extend_from_slice(b"ABCD");
        let mut out = vec![0u8; 128];
        out.extend_from_slice(b"DICM");
        let mut meta = DataSet::new();
        meta.put_text(TRANSFER_SYNTAX_UID, Vr::UI, EXPLICIT_VR_LITTLE_ENDIAN);
        encode_dataset(&meta, &mut out).unwrap();
        out.extend_from_slice(&[0x99, 0x00, 0x10, 0x10, b'U', b'N', 0, 0]);
        out.extend_from_slice(&UNDEFINED.to_le_bytes());
        out.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0]);
        out.extend_from_slice(&(inner.len() as u32).to_le_bytes());
        out.extend_from_slice(&inner);
        out.extend_from_slice(&[0xFE, 0xFF, 0xDD, 0xE0, 0, 0, 0, 0]);
        let file = read(&out).unwrap();
        let items = file.dataset.items(Tag(0x0099, 0x1010));
        assert_eq!(items[0].text(Tag(0x0008, 0x0100)).as_deref(), Some("ABCD"));
    }

    #[test]
    fn truncated_files_are_errors_at_every_length() {
        let bytes = write(&META, &sample()).unwrap();
        let full = read(&bytes).unwrap();
        for end in 0..bytes.len() {
            // Never a panic; a cut inside a value or a sequence is an error,
            // a cut between top-level elements reads fewer elements.
            if let Ok(file) = read(&bytes[..end]) {
                assert!(file.dataset.len() < full.dataset.len() || file.meta != full.meta);
            }
        }
    }

    #[test]
    fn damaged_files_never_panic() {
        let bytes = write(&META, &sample()).unwrap();
        // Deterministic pseudo-random corruption of one to four bytes.
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let mut damaged = bytes.clone();
            for _ in 0..(next() % 4 + 1) {
                let at = 128 + (next() as usize) % (damaged.len() - 128);
                damaged[at] = next() as u8;
            }
            let _ = read(&damaged);
        }
    }

    #[test]
    fn hostile_lengths_and_nesting_are_refused_without_allocating() {
        let mut out = vec![0u8; 128];
        out.extend_from_slice(b"DICM");
        let mut meta = DataSet::new();
        meta.put_text(TRANSFER_SYNTAX_UID, Vr::UI, EXPLICIT_VR_LITTLE_ENDIAN);
        encode_dataset(&meta, &mut out).unwrap();
        let header = out.clone();
        // A 4 GB coordinate array in a 200-byte file.
        out.extend_from_slice(&[0x66, 0x00, 0x16, 0x00, b'O', b'F', 0, 0]);
        out.extend_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        out.extend_from_slice(&[0; 16]);
        let error = read(&out).unwrap_err();
        assert!(error.contains("only 16 remain"), "{error}");
        // A million nested sequences.
        let mut deep = header.clone();
        for _ in 0..100_000 {
            deep.extend_from_slice(&[0x6A, 0x00, 0x02, 0x00, b'S', b'Q', 0, 0]);
            deep.extend_from_slice(&UNDEFINED.to_le_bytes());
            deep.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0]);
            deep.extend_from_slice(&UNDEFINED.to_le_bytes());
        }
        let error = read(&deep).unwrap_err();
        assert!(error.contains("nested"), "{error}");
        // An unknown VR, a stray delimiter, items outside a sequence.
        for bad in [
            &[0x10, 0x00, 0x10, 0x00, b'Z', b'Z', 0, 0][..],
            &[0xFE, 0xFF, 0xDD, 0xE0, 0, 0, 0, 0][..],
            &[0xFE, 0xFF, 0x0D, 0xE0, 0, 0, 0, 0][..],
            &[0xFE, 0xFF, 0x00, 0xE0, 0, 0, 0, 0][..],
        ] {
            let mut file = header.clone();
            file.extend_from_slice(bad);
            assert!(read(&file).is_err(), "{bad:?}");
        }
        // Not DICOM, or unsupported encodings.
        assert!(read(b"DICM").unwrap_err().contains("not a DICOM file"));
        for syntax in [EXPLICIT_VR_BIG_ENDIAN, DEFLATED_EXPLICIT_VR_LITTLE_ENDIAN] {
            let mut file = vec![0u8; 128];
            file.extend_from_slice(b"DICM");
            let mut meta = DataSet::new();
            meta.put_text(TRANSFER_SYNTAX_UID, Vr::UI, syntax);
            encode_dataset(&meta, &mut file).unwrap();
            assert!(read(&file).is_err(), "{syntax}");
        }
        let mut no_meta = vec![0u8; 128];
        no_meta.extend_from_slice(b"DICM");
        no_meta.extend_from_slice(&[0x08, 0x00, 0x16, 0x00, b'U', b'I', 2, 0, b'1', 0]);
        assert!(read(&no_meta).unwrap_err().contains("meta"));
    }

    #[test]
    fn element_count_is_limited() {
        let mut out = vec![0u8; 128];
        out.extend_from_slice(b"DICM");
        let mut meta = DataSet::new();
        meta.put_text(TRANSFER_SYNTAX_UID, Vr::UI, EXPLICIT_VR_LITTLE_ENDIAN);
        encode_dataset(&meta, &mut out).unwrap();
        // Empty items are 8 bytes each: a sequence of more than the limit.
        out.extend_from_slice(&[0x6A, 0x00, 0x02, 0x00, b'S', b'Q', 0, 0]);
        out.extend_from_slice(&UNDEFINED.to_le_bytes());
        for _ in 0..=MAX_ELEMENTS {
            out.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0, 0, 0, 0, 0]);
        }
        out.extend_from_slice(&[0xFE, 0xFF, 0xDD, 0xE0, 0, 0, 0, 0]);
        let error = read(&out).unwrap_err();
        assert!(error.contains("more than"), "{error}");
    }

    #[test]
    fn values_are_interpreted_defensively() {
        let mut ds = DataSet::new();
        ds.put_bytes(Tag(0x0066, 0x0016), Vr::OF, vec![0; 6]);
        assert!(ds.f32s(Tag(0x0066, 0x0016)).is_err());
        assert!(ds.u64s(Tag(0x0066, 0x0016)).is_err());
        assert_eq!(ds.f32s(Tag(0x0066, 0x0022)).unwrap(), None);
        ds.put_text(Tag(0x0020, 0x0013), Vr::IS, " 12 ");
        assert_eq!(ds.uint(Tag(0x0020, 0x0013)), Some(12));
        ds.put_bytes(Tag(0x0040, 0xA180), Vr::US, vec![1]);
        assert_eq!(ds.uint(Tag(0x0040, 0xA180)), None);
        ds.put_text(Tag(0x0008, 0x0008), Vr::CS, "ORIGINAL\\PRIMARY\\VOLUME ");
        assert_eq!(
            ds.texts(Tag(0x0008, 0x0008)),
            ["ORIGINAL", "PRIMARY", "VOLUME"]
        );
        // ISO 8859-1 text that is not UTF-8.
        ds.put_bytes(
            Tag(0x0010, 0x0010),
            Vr::PN,
            vec![b'M', 0xFC, b'l', b'l', b'e', b'r'],
        );
        assert_eq!(ds.text(Tag(0x0010, 0x0010)).as_deref(), Some("Müller"));
        assert!(ds.items(Tag(0x0010, 0x0010)).is_empty());
    }

    #[test]
    fn values_too_long_for_their_vr_are_refused() {
        let mut ds = DataSet::new();
        ds.put_text(Tag(0x0010, 0x0010), Vr::LO, &"x".repeat(70_000));
        assert!(write(&META, &ds).is_err());
    }
}
