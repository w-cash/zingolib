use pepper_sync::keys::transparent::TransparentScope;
use zip32::AccountId;

/// Describes spend selection restrictions that may be applied during proposal building.
#[derive(Debug, Clone, Default)]
pub struct SpendRestriction {
    pub shielded: Option<ShieldedAddressRestriction>,
    pub transparent: Option<TransparentAddressRestriction>,
}

/// Restricts shielded spending to notes received at specific unified-address indexes.
#[derive(Debug, Clone)]
pub struct ShieldedAddressRestriction {
    pub account: AccountId,
    pub start_index: u32,
    pub max_index: Option<u32>,
}

impl ShieldedAddressRestriction {
    pub fn new(account: AccountId, start_index: u32, max_index: Option<u32>) -> Self {
        Self {
            account,
            start_index,
            max_index,
        }
    }

    pub fn contains(&self, address_index: u32) -> bool {
        address_index >= self.start_index
            && address_index <= self.max_index.unwrap_or(self.start_index)
    }
}

/// Restricts transparent spending to a range of transparent child indexes.
#[derive(Debug, Clone)]
pub struct TransparentAddressRestriction {
    pub account: AccountId,
    pub scope: TransparentScope,
    pub start_index: u32,
    pub max_index: Option<u32>,
}

impl TransparentAddressRestriction {
    pub fn new(
        account: AccountId,
        scope: TransparentScope,
        start_index: u32,
        max_index: Option<u32>,
    ) -> Self {
        Self {
            account,
            scope,
            start_index,
            max_index,
        }
    }

    pub fn contains(&self, address_index: u32) -> bool {
        address_index >= self.start_index
            && address_index <= self.max_index.unwrap_or(self.start_index)
    }
}
