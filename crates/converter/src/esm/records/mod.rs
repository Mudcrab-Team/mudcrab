pub mod aact;
pub mod achr;
pub mod clfm;
pub mod record_type;
pub mod tes4;
pub mod tree;

#[derive(Debug, Clone)]
pub struct RawRecord {
    pub form_id: u32,
    pub record_type: [u8; 4],
    pub flags: u32,
    pub subrecords: Vec<(Vec<u8>, Vec<u8>)>,
    pub cell_form_id: Option<u32>,
    pub worldspace_form_id: Option<u32>,
    pub load_order: u32,
}

impl RawRecord {
    pub fn is_deleted(&self) -> bool {
        self.flags & 0x00000020 != 0
    }
}

pub trait EsmRecord: Sized {
    const RECORD_TYPE: &'static [u8; 4];
    fn parse(raw: &RawRecord) -> Option<Self>;
}
