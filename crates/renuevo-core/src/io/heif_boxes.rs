//! Lectura mínima de las cajas de un HEIF: si la imagen principal lleva giro (`irot`) o espejo
//! (`imir`), y si hay un mapa de tonos HDR (`tmap`, ISO 21496-1). `libheif-rs` no expone estas
//! propiedades sin código `unsafe`, que este proyecto no usa.

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeifLayout {
    /// Giro de la imagen principal en cuartos de vuelta antihorarios (`irot`, 0..=3).
    pub rotation: u8,
    pub mirrored: bool,
    pub has_tone_map: bool,
}

impl HeifLayout {
    pub fn rotated_or_mirrored(&self) -> bool {
        self.rotation != 0 || self.mirrored
    }

    /// 90° o 270°: ancho y alto (y sus DPI) se intercambian.
    pub fn quarter_turn(&self) -> bool {
        self.rotation % 2 == 1
    }
}

/// Lee lo que pueda: una caja dañada o ausente no impide leer las demás.
pub(crate) fn layout(file: &[u8]) -> Option<HeifLayout> {
    let meta = find(file, b"meta")?.get(4..)?; // caja «completa»: versión + flags
    let mut out = HeifLayout {
        has_tone_map: find(meta, b"iinf")
            .is_some_and(|iinf| item_types(iinf).any(|t| &t == b"tmap")),
        ..HeifLayout::default()
    };
    let (Some(primary), Some(iprp)) = (primary_item(meta), find(meta, b"iprp")) else {
        return Some(out);
    };
    let properties: Vec<Bx> = find(iprp, b"ipco").map_or_else(Vec::new, |c| boxes(c).collect());
    for ipma in boxes(iprp).filter(|b| &b.kind == b"ipma") {
        // Una `ipma` mal formada se salta; sus asociaciones válidas hasta el error cuentan.
        for index in associations(ipma.body, primary) {
            match properties.get(index.wrapping_sub(1)) {
                Some(p) if &p.kind == b"irot" => {
                    out.rotation = p.body.first().map_or(0, |a| a & 3);
                }
                Some(p) if &p.kind == b"imir" => out.mirrored = true,
                _ => {}
            }
        }
    }
    Some(out)
}

fn primary_item(meta: &[u8]) -> Option<u32> {
    let mut r = Reader::new(find(meta, b"pitm")?);
    let version = r.u8()?;
    r.skip(3)?;
    if version == 0 {
        Some(u32::from(r.u16()?))
    } else {
        r.u32()
    }
}

/// Índices (desde 1) de las propiedades asociadas a `item` en una caja `ipma`. Si la caja está
/// truncada, devuelve las que se pudieron leer.
fn associations(ipma: &[u8], item: u32) -> Vec<usize> {
    let mut out = Vec::new();
    let mut r = Reader::new(ipma);
    let header = (|| Some((r.u8()?, r.u24()?, r.u32()?)))();
    let Some((version, flags, entries)) = header else {
        return out;
    };
    for _ in 0..entries {
        let id = if version < 1 {
            r.u16().map(u32::from)
        } else {
            r.u32()
        };
        let (Some(id), Some(count)) = (id, r.u8()) else {
            break;
        };
        for _ in 0..count {
            let index = if flags & 1 != 0 {
                r.u16().map(|v| usize::from(v & 0x7FFF))
            } else {
                r.u8().map(|v| usize::from(v & 0x7F))
            };
            let Some(index) = index else {
                return out;
            };
            if id == item && index > 0 {
                out.push(index);
            }
        }
    }
    out
}

/// Tipos de los elementos declarados en `iinf` (cajas `infe` de versión 2 y 3).
fn item_types(iinf: &[u8]) -> impl Iterator<Item = [u8; 4]> + '_ {
    let start = if iinf.first() == Some(&0) { 6 } else { 8 };
    boxes(iinf.get(start..).unwrap_or_default())
        .filter(|b| &b.kind == b"infe")
        .filter_map(|b| {
            let offset = match b.body.first()? {
                2 => 4 + 2 + 2,
                3 => 4 + 4 + 2,
                _ => return None,
            };
            b.body.get(offset..offset + 4)?.try_into().ok()
        })
}

struct Bx<'a> {
    kind: [u8; 4],
    body: &'a [u8],
}

fn find<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data).find(|b| &b.kind == kind).map(|b| b.body)
}

fn boxes(data: &[u8]) -> impl Iterator<Item = Bx<'_>> {
    let mut rest = data;
    std::iter::from_fn(move || {
        let mut r = Reader::new(rest);
        let size32 = r.u32()?;
        let kind: [u8; 4] = r.take(4)?.try_into().ok()?;
        let (header, size) = match size32 {
            0 => (8, rest.len()),
            1 => (16, usize::try_from(r.u64()?).ok()?),
            s => (8, usize::try_from(s).ok()?),
        };
        if size < header || size > rest.len() {
            return None;
        }
        let body = &rest[header..size];
        rest = &rest[size..];
        Some(Bx { kind, body })
    })
}

struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.data.split_at_checked(n)?;
        self.data = tail;
        Some(head)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u24(&mut self) -> Option<u32> {
        let b = self.take(3)?;
        Some(u32::from_be_bytes([0, b[0], b[1], b[2]]))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    /// HEIF mínimo: elemento principal 1 con las propiedades `props` asociadas.
    fn heif(props: &[Vec<u8>], with_tmap: bool) -> Vec<u8> {
        let pitm = bx(b"pitm", &[0, 0, 0, 0, 0, 1]);
        let ipco = bx(b"ipco", &props.concat());
        let mut ipma_body = vec![0, 0, 0, 0, 0, 0, 0, 1, 0, 1, props.len() as u8];
        ipma_body.extend((1..=props.len() as u8).map(|i| 0x80 | i));
        let ipma = bx(b"ipma", &ipma_body);
        let iprp = bx(b"iprp", &[ipco, ipma].concat());
        let mut infe = vec![bx(
            b"infe",
            &[2, 0, 0, 0, 0, 1, 0, 0, b'h', b'v', b'c', b'1'],
        )];
        if with_tmap {
            infe.push(bx(
                b"infe",
                &[2, 0, 0, 0, 0, 2, 0, 0, b't', b'm', b'a', b'p'],
            ));
        }
        let mut iinf_body = vec![0, 0, 0, 0];
        iinf_body.extend_from_slice(&(infe.len() as u16).to_be_bytes());
        iinf_body.extend(infe.concat());
        let iinf = bx(b"iinf", &iinf_body);
        let mut meta_body = vec![0, 0, 0, 0];
        meta_body.extend([pitm, iinf, iprp].concat());
        [bx(b"ftyp", b"heic\0\0\0\0mif1"), bx(b"meta", &meta_body)].concat()
    }

    #[test]
    fn detects_rotation_mirror_and_tone_map() {
        let ispe = bx(b"ispe", &[0; 12]);
        let none = layout(&heif(std::slice::from_ref(&ispe), false)).unwrap();
        assert_eq!(none, HeifLayout::default());

        let rot180 = layout(&heif(&[ispe.clone(), bx(b"irot", &[2])], false)).unwrap();
        assert!(rot180.rotated_or_mirrored() && !rot180.quarter_turn());
        let rot90 = layout(&heif(&[ispe.clone(), bx(b"irot", &[1])], false)).unwrap();
        assert!(rot90.quarter_turn());
        let rot0 = layout(&heif(&[ispe.clone(), bx(b"irot", &[0])], false)).unwrap();
        assert!(!rot0.rotated_or_mirrored());
        let mirror = layout(&heif(&[bx(b"imir", &[1]), ispe], true)).unwrap();
        assert!(mirror.rotated_or_mirrored() && !mirror.quarter_turn());
        assert!(mirror.has_tone_map);
    }

    #[test]
    fn garbage_does_not_panic() {
        assert_eq!(layout(b""), None);
        assert_eq!(layout(&[0xFF; 64]), None);
        let mut truncated = heif(&[bx(b"irot", &[1])], true);
        truncated.truncate(truncated.len() - 5);
        let _ = layout(&truncated);
    }

    /// Una `ipma` con un número de entradas absurdo no tumba lo demás: el giro leído antes del
    /// error y el `tmap` (que está en otra caja) siguen contando.
    #[test]
    fn corrupt_ipma_keeps_what_was_read() {
        let mut file = heif(&[bx(b"irot", &[3])], true);
        let at = file.windows(4).position(|w| w == b"ipma").unwrap() + 4 + 4;
        file[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        let out = layout(&file).unwrap();
        assert_eq!(out.rotation, 3);
        assert!(out.has_tone_map);
    }
}
