#[derive(Default)]
pub struct CTableH {
    pub table_log: u16,
    pub fast_mode: u16,
}

#[derive(Default, Clone)]
pub struct CTableEntry {
    pub new_state: u16,
    pub symbol: u8,
    pub nb_bits: u8,
}

#[derive(Default)]
pub struct CTable {
    pub header: CTableH,
    pub table: Vec<CTableEntry>,
}
