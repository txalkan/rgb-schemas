use aluvm::isa::Instr;
use aluvm::library::{Lib, LibSite};
use amplify::confinement::Confined;
use rgbstd::contract::{
    AssignmentsFilter, ContractData, FungibleAllocation, IssuerWrapper, LinkError,
    LinkableIssuerWrapper, LinkableSchemaWrapper, SchemaWrapper,
};
use rgbstd::persistence::{ContractStateRead, MemContract};
use rgbstd::rgbcore::stl::rgb_contract_id_stl;
use rgbstd::schema::{
    AssignmentDetails, FungibleType, GenesisSchema, GlobalStateSchema, Occurrences,
    OwnedStateSchema, Schema, TransitionSchema,
};
use rgbstd::stl::{AssetSpec, ContractTerms, Details, RejectListUrl, StandardTypes};
use rgbstd::validation::Scripts;
use rgbstd::vm::RgbIsa;
use rgbstd::{rgbasm, Amount, ContractId, GlobalDetails, MetaDetails, SchemaId, TransitionDetails};
use strict_types::{StrictVal, TypeSystem};

use crate::{
    ERRNO_INFLATION_EXCEEDS_ALLOWANCE, ERRNO_INFLATION_MISMATCH, ERRNO_ISSUED_MISMATCH,
    ERRNO_NON_EQUAL_IN_OUT, GS_EVM_CHAIN_ID, GS_EVM_CLAIM_MINT_CONTRACT, GS_ISSUED_SUPPLY,
    GS_LINKED_FROM_CONTRACT, GS_LINKED_TO_CONTRACT, GS_MAX_SUPPLY, GS_NOMINAL, GS_REJECT_LIST_URL,
    GS_TERMS, MS_ALLOWED_INFLATION, MS_CLAIM_AMOUNT, MS_CLAIM_CHAIN_ID, MS_CLAIM_CONTRACT,
    MS_CLAIM_EVENT_LOG_INDEX, MS_CLAIM_EVENT_TX_HASH, MS_CLAIM_NULLIFIER, MS_CLAIM_PROOF, OS_ASSET,
    OS_CLAIM_BATON, OS_INFLATION, OS_LINK, TS_BURN, TS_CLAIM_MINT, TS_GRANT_BATON, TS_INFLATION,
    TS_LINK, TS_TRANSFER,
};

pub const BIFA_SCHEMA_ID: SchemaId = SchemaId::from_array([
    0x0d, 0x4d, 0x7b, 0xab, 0xcc, 0x66, 0x95, 0x28, 0xbf, 0xd4, 0x0d, 0x7d, 0xf2, 0xb1, 0xc9, 0xe3,
    0x87, 0x11, 0xb7, 0xc7, 0x84, 0x17, 0x94, 0x4b, 0x75, 0xba, 0x87, 0x49, 0x7c, 0x6e, 0xac, 0xb0,
]);

pub(crate) fn bifa_lib_genesis() -> Lib {
    #[allow(clippy::diverging_sub_expression)]
    let code = rgbasm! {
        // Set common offsets
        put     a8[1],0;
        put     a16[0],0;

        // Check reported issued supply against sum of asset allocations in output
        put     a8[0],ERRNO_ISSUED_MISMATCH;  // set errno
        ldg     GS_ISSUED_SUPPLY,a8[1],s16[0];  // read issued supply global state
        extr    s16[0],a64[0],a16[0];  // and store it in a64[0]
        sas     OS_ASSET;  // check sum of assets assignments in output equals a64[0]
        test;

        // Check that sum of inflation rights = max supply - issued supply
        put     a8[0],ERRNO_INFLATION_MISMATCH;  // set errno
        ldg     GS_MAX_SUPPLY,a8[1],s16[1];  // read max supply global state
        extr    s16[1],a64[1],a16[0];  // and store it in a64[1]
        sub.uc  a64[1],a64[0];  // issued supply is still in a64[0], result overwrites a64[0]
        test;  // fails if result is <0
        sas     OS_INFLATION;  // check sum of inflation rights in output equals a64[0]
        test;

        ret;
    };
    Lib::assemble::<Instr<RgbIsa<MemContract>>>(&code)
        .expect("wrong inflatable asset genesis valdiation script")
}

pub(crate) fn bifa_lib_transfer() -> Lib {
    let code = rgbasm! {
        // Checking that the sum of inputs is equal to the sum of outputs
        put     a8[0],ERRNO_NON_EQUAL_IN_OUT;  // set errno
        svs     OS_ASSET;  // verify sum
        test;  // check it didn't fail
        svs     OS_INFLATION;  // verify sum
        test;  // check it didn't fail

        // Link rights validation
        put     a8[0],ERRNO_NON_EQUAL_IN_OUT;  // set errno
        cnp     OS_LINK,a16[0];  // count input link rights
        cns     OS_LINK,a16[1];  // count output link rights
        eq.n    a16[0],a16[1];  // check if input_count == output_count
        test;  // fail if output_count != input_count

        ret;  // return execution flow
    };
    Lib::assemble::<Instr<RgbIsa<MemContract>>>(&code).expect("wrong transfer validation script")
}

pub(crate) fn bifa_lib_inflation() -> Lib {
    #[allow(clippy::diverging_sub_expression)]
    let code = rgbasm! {
        // Set common offsets
        put     a8[1],0;
        put     a16[0],0;

        // Check reported issued supply equals sum of asset allocations in output
        put     a8[0],ERRNO_ISSUED_MISMATCH;  // set errno
        ldg     GS_ISSUED_SUPPLY,a8[1],s16[0];  // read issued supply global state
        extr    s16[0],a64[0],a16[0];  // and store it in a64[0]
        sas     OS_ASSET;  // check sum of asset allocations in output equals issued_supply
        test;
        cpy     a64[0],a64[1];  // store issued supply in a64[1] for later

        // Check reported allowed inflation equals sum of inflation rights in output
        put     a8[0],ERRNO_INFLATION_MISMATCH;  // set errno
        ldm     MS_ALLOWED_INFLATION,s16[0];  // read allowed inflation global state
        extr    s16[0],a64[0],a16[0];  // and store it in a64[0]
        sas     OS_INFLATION;  // check sum of inflation rights in output equals a64[0]
        test;

        // Check that input inflation rights equals issued supply + allowed inflation
        put     a8[0],ERRNO_INFLATION_EXCEEDS_ALLOWANCE;
        add.uc  a64[1],a64[0];  // result is stored in a64[0]
        test;  // fails in case of an overflow
        sps     OS_INFLATION;  // check sum of inflation rights in input equals a64[0]
        test;

        ret;
    };
    Lib::assemble::<Instr<RgbIsa<MemContract>>>(&code).expect("wrong inflation validation script")
}

pub(crate) fn bifa_lib_claim_mint() -> Lib {
    #[allow(clippy::diverging_sub_expression)]
    let code = rgbasm! {
        // Set common offsets
        put     a8[1],0;
        put     a16[0],0;

        // Check reported issued supply equals sum of minted assets in output
        put     a8[0],ERRNO_ISSUED_MISMATCH;  // set errno
        ldg     GS_ISSUED_SUPPLY,a8[1],s16[0];  // read issued supply global state
        extr    s16[0],a64[0],a16[0];  // and store it in a64[0]
        sas     OS_ASSET;  // check sum of asset allocations in output equals issued_supply
        test;

        // Enforce claim baton conservation (1-in/1-out via schema + equality guard)
        put     a8[0],ERRNO_NON_EQUAL_IN_OUT;  // set errno
        cnp     OS_CLAIM_BATON,a16[0];  // count input claim batons
        cns     OS_CLAIM_BATON,a16[1];  // count output claim batons
        eq.n    a16[0],a16[1];  // check if input_count == output_count
        test;  // fail if output_count != input_count

        ret;
    };
    Lib::assemble::<Instr<RgbIsa<MemContract>>>(&code).expect("wrong claim mint validation script")
}

fn bifa_standard_types() -> StandardTypes { StandardTypes::with(rgb_contract_id_stl()) }

fn bifa_schema() -> Schema {
    let types = bifa_standard_types();

    let alu_id_transfer = bifa_lib_transfer().id();

    Schema {
        ffv: zero!(),
        name: tn!("BridgedInflatableFungibleAsset"),
        meta_types: tiny_bmap! {
            MS_ALLOWED_INFLATION => MetaDetails {
                sem_id: types.get("RGBContract.Amount"),
                name: fname!("allowedInflation"),
            },
            MS_CLAIM_AMOUNT => MetaDetails {
                sem_id: types.get("RGBContract.Amount"),
                name: fname!("claimAmount"),
            },
            MS_CLAIM_CHAIN_ID => MetaDetails {
                sem_id: types.get("RGBContract.Amount"),
                name: fname!("claimChainId"),
            },
            MS_CLAIM_CONTRACT => MetaDetails {
                sem_id: types.get("RGBContract.Details"),
                name: fname!("claimContract"),
            },
            MS_CLAIM_EVENT_LOG_INDEX => MetaDetails {
                sem_id: types.get("RGBContract.Amount"),
                name: fname!("claimEventLogIndex"),
            },
            MS_CLAIM_EVENT_TX_HASH => MetaDetails {
                sem_id: types.get("RGBContract.Details"),
                name: fname!("claimEventTxHash"),
            },
            MS_CLAIM_NULLIFIER => MetaDetails {
                sem_id: types.get("RGBContract.Details"),
                name: fname!("claimNullifier"),
            },
            MS_CLAIM_PROOF => MetaDetails {
                sem_id: types.get("RGBContract.Details"),
                name: fname!("claimProof"),
            }
        },
        global_types: tiny_bmap! {
            GS_NOMINAL => GlobalDetails {
                global_state_schema: GlobalStateSchema::once(types.get("RGBContract.AssetSpec")),
                name: fname!("spec"),
            },
            GS_TERMS => GlobalDetails {
                global_state_schema: GlobalStateSchema::once(types.get("RGBContract.ContractTerms")),
                name: fname!("terms"),
            },
            GS_ISSUED_SUPPLY => GlobalDetails {
                global_state_schema: GlobalStateSchema::many(types.get("RGBContract.Amount")),
                name: fname!("issuedSupply"),
            },
            GS_MAX_SUPPLY => GlobalDetails {
                global_state_schema: GlobalStateSchema::once(types.get("RGBContract.Amount")),
                name: fname!("maxSupply"),
            },
            GS_REJECT_LIST_URL => GlobalDetails {
                global_state_schema: GlobalStateSchema::once(types.get("RGBContract.RejectListUrl")),
                name: fname!("rejectListUrl"),
            },
            GS_LINKED_FROM_CONTRACT => GlobalDetails {
                global_state_schema: GlobalStateSchema::once(types.get("RGBCommit.ContractId")),
                name: fname!("linkedFromContract"),
            },
            GS_LINKED_TO_CONTRACT => GlobalDetails {
                global_state_schema: GlobalStateSchema::once(types.get("RGBCommit.ContractId")),
                name: fname!("linkedToContract"),
            },
            GS_EVM_CHAIN_ID => GlobalDetails {
                global_state_schema: GlobalStateSchema::once(types.get("RGBContract.Amount")),
                name: fname!("evmChainId"),
            },
            GS_EVM_CLAIM_MINT_CONTRACT => GlobalDetails {
                // Stored as a structured detail value (e.g. 0x-prefixed address string).
                global_state_schema: GlobalStateSchema::once(types.get("RGBContract.Details")),
                name: fname!("evmClaimMintContract"),
            },
        },
        owned_types: tiny_bmap! {
            OS_ASSET => AssignmentDetails {
                owned_state_schema: OwnedStateSchema::Fungible(FungibleType::Unsigned64Bit),
                name: fname!("assetOwner"),
                default_transition: TS_TRANSFER,
            },
            OS_CLAIM_BATON => AssignmentDetails {
                owned_state_schema: OwnedStateSchema::Declarative,
                name: fname!("claimMintBaton"),
                default_transition: TS_GRANT_BATON,
            },
            OS_INFLATION => AssignmentDetails {
                owned_state_schema: OwnedStateSchema::Fungible(FungibleType::Unsigned64Bit),
                name: fname!("inflationAllowance"),
                default_transition: TS_TRANSFER
            },
            OS_LINK => AssignmentDetails {
                owned_state_schema: OwnedStateSchema::Declarative,
                name: fname!("linkRight"),
                default_transition: TS_TRANSFER,
            }
        },
        genesis: GenesisSchema {
            metadata: none!(),
            globals: tiny_bmap! {
                GS_NOMINAL => Occurrences::Once,
                GS_TERMS => Occurrences::Once,
                GS_ISSUED_SUPPLY => Occurrences::Once,
                GS_MAX_SUPPLY => Occurrences::Once,
                GS_REJECT_LIST_URL => Occurrences::NoneOrOnce,
                GS_LINKED_FROM_CONTRACT => Occurrences::NoneOrOnce,
                GS_EVM_CHAIN_ID => Occurrences::Once,
                GS_EVM_CLAIM_MINT_CONTRACT => Occurrences::Once,
            },
            assignments: tiny_bmap! {
                OS_ASSET => Occurrences::NoneOrMore,
                OS_CLAIM_BATON => Occurrences::NoneOrMore,
                OS_INFLATION => Occurrences::NoneOrMore,
                OS_LINK => Occurrences::NoneOrOnce
            },
            validator: Some(LibSite::with(0, bifa_lib_genesis().id())),
        },
        transitions: tiny_bmap! {
            TS_TRANSFER => TransitionDetails {
                transition_schema: TransitionSchema {
                    metadata: none!(),
                    globals: none!(),
                    inputs: tiny_bmap! {
                        OS_ASSET => Occurrences::NoneOrMore,
                        OS_INFLATION => Occurrences::NoneOrMore,
                        OS_LINK => Occurrences::NoneOrOnce,
                    },
                    assignments: tiny_bmap! {
                        OS_ASSET => Occurrences::NoneOrMore,
                        OS_INFLATION => Occurrences::NoneOrMore,
                        OS_LINK => Occurrences::NoneOrOnce,
                    },
                    validator: Some(LibSite::with(0, alu_id_transfer))
                },
                name: fname!("transfer"),
            },
            TS_INFLATION => TransitionDetails {
                transition_schema: TransitionSchema {
                    metadata: tiny_bset![MS_ALLOWED_INFLATION],
                    globals: tiny_bmap! {
                        GS_ISSUED_SUPPLY => Occurrences::Once,
                    },
                    inputs: tiny_bmap! {
                        OS_INFLATION => Occurrences::OnceOrMore
                    },
                    assignments: tiny_bmap! {
                        OS_ASSET => Occurrences::OnceOrMore,
                        OS_INFLATION => Occurrences::NoneOrMore
                    },
                    validator: Some(LibSite::with(0, bifa_lib_inflation().id()))
                },
                name: fname!("inflate"),
            },
            TS_GRANT_BATON => TransitionDetails {
                transition_schema: TransitionSchema {
                    metadata: none!(),
                    globals: none!(),
                    inputs: tiny_bmap! {
                        OS_CLAIM_BATON => Occurrences::Once,
                    },
                    assignments: tiny_bmap! {
                        OS_CLAIM_BATON => Occurrences::OnceOrMore,
                    },
                    validator: None
                },
                name: fname!("grantBaton"),
            },
            TS_CLAIM_MINT => TransitionDetails {
                transition_schema: TransitionSchema {
                    metadata: tiny_bset![
                        MS_CLAIM_AMOUNT,
                        MS_CLAIM_CHAIN_ID,
                        MS_CLAIM_CONTRACT,
                        MS_CLAIM_EVENT_LOG_INDEX,
                        MS_CLAIM_EVENT_TX_HASH,
                        MS_CLAIM_NULLIFIER,
                        MS_CLAIM_PROOF
                    ],
                    globals: tiny_bmap! {
                        GS_ISSUED_SUPPLY => Occurrences::Once,
                    },
                    inputs: tiny_bmap! {
                        OS_CLAIM_BATON => Occurrences::Once,
                    },
                    assignments: tiny_bmap! {
                        OS_ASSET => Occurrences::OnceOrMore,
                        OS_CLAIM_BATON => Occurrences::Once,
                    },
                    validator: Some(LibSite::with(0, bifa_lib_claim_mint().id()))
                },
                name: fname!("claimMint"),
            },
            TS_BURN => TransitionDetails {
                transition_schema: TransitionSchema {
                    metadata: none!(),
                    globals: none!(),
                    inputs: tiny_bmap! {
                        OS_ASSET => Occurrences::NoneOrMore,
                        OS_INFLATION => Occurrences::NoneOrMore,
                        OS_LINK => Occurrences::NoneOrOnce,
                    },
                    assignments: none!(),
                    validator: None
                },
                name: fname!("burn"),
            },
            TS_LINK => TransitionDetails {
                transition_schema: TransitionSchema {
                    metadata: none!(),
                    globals: tiny_bmap! {
                        GS_LINKED_TO_CONTRACT => Occurrences::Once,
                    },
                    inputs: tiny_bmap! {
                        OS_LINK => Occurrences::Once,
                    },
                    assignments: none!(),
                    validator: None
                },
                name: fname!("link"),
            },
        },
        default_assignment: Some(OS_ASSET),
    }
}

#[derive(Default)]
pub struct BridgedInflatableFungibleAsset;

impl IssuerWrapper for BridgedInflatableFungibleAsset {
    type Wrapper<S: ContractStateRead> = BifaWrapper<S>;

    fn schema() -> Schema { bifa_schema() }

    fn types() -> TypeSystem { bifa_standard_types().type_system(bifa_schema()) }

    fn scripts() -> Scripts {
        let alu_lib_genesis = bifa_lib_genesis();
        let alu_id_genesis = alu_lib_genesis.id();

        let alu_lib_transfer = bifa_lib_transfer();
        let alu_id_transfer = alu_lib_transfer.id();

        let alu_lib_inflation = bifa_lib_inflation();
        let alu_id_inflation = alu_lib_inflation.id();

        let alu_lib_claim_mint = bifa_lib_claim_mint();
        let alu_id_claim_mint = alu_lib_claim_mint.id();

        Confined::from_checked(bmap! {
            alu_id_genesis => alu_lib_genesis,
            alu_id_transfer => alu_lib_transfer,
            alu_id_inflation => alu_lib_inflation,
            alu_id_claim_mint => alu_lib_claim_mint,
        })
    }
}

impl LinkableIssuerWrapper for BridgedInflatableFungibleAsset {
    type Wrapper<S: ContractStateRead> = BifaWrapper<S>;
}

#[derive(Clone, Eq, PartialEq, Debug, From)]
pub struct BifaWrapper<S: ContractStateRead>(ContractData<S>);

impl<S: ContractStateRead> SchemaWrapper<S> for BifaWrapper<S> {
    fn with(data: ContractData<S>) -> Self {
        if data.schema.schema_id() != BIFA_SCHEMA_ID {
            panic!("the provided schema is not BIFA");
        }
        Self(data)
    }
}

impl<S: ContractStateRead> BifaWrapper<S> {
    pub fn spec(&self) -> AssetSpec {
        let strict_val = &self
            .0
            .global("spec")
            .next()
            .expect("BIFA requires global state `spec` to have at least one item");
        AssetSpec::from_strict_val_unchecked(strict_val)
    }

    pub fn contract_terms(&self) -> ContractTerms {
        let strict_val = &self
            .0
            .global("terms")
            .next()
            .expect("BIFA requires global state `terms` to have at least one item");
        ContractTerms::from_strict_val_unchecked(strict_val)
    }

    pub fn reject_list_url(&self) -> Option<RejectListUrl> {
        self.0
            .global("rejectListUrl")
            .next()
            .map(|strict_val| RejectListUrl::from_strict_val_unchecked(&strict_val))
    }

    pub fn evm_chain_id(&self) -> u64 {
        let strict_val = &self
            .0
            .global("evmChainId")
            .next()
            .expect("BIFA requires global state `evmChainId` to have at least one item");
        Amount::from_strict_val_unchecked(strict_val).value()
    }

    pub fn evm_claim_mint_contract(&self) -> Details {
        let strict_val =
            &self.0.global("evmClaimMintContract").next().expect(
                "BIFA requires global state `evmClaimMintContract` to have at least one item",
            );
        Details::from_strict_val_unchecked(strict_val)
    }

    fn issued_supply(&self) -> impl Iterator<Item = Amount> + '_ {
        self.0
            .global("issuedSupply")
            .map(|amount| Amount::from_strict_val_unchecked(&amount))
    }

    pub fn total_issued_supply(&self) -> Amount { self.issued_supply().sum() }

    pub fn issuance_amounts(&self) -> Vec<Amount> { self.issued_supply().collect::<Vec<_>>() }

    pub fn max_supply(&self) -> Amount {
        self.0
            .global("maxSupply")
            .map(|amount| Amount::from_strict_val_unchecked(&amount))
            .sum()
    }

    pub fn allocations<'c>(
        &'c self,
        filter: impl AssignmentsFilter + 'c,
    ) -> impl Iterator<Item = FungibleAllocation> + 'c {
        self.0.fungible_raw(OS_ASSET, filter).unwrap()
    }

    pub fn inflation_allocations<'c>(
        &'c self,
        filter: impl AssignmentsFilter + 'c,
    ) -> impl Iterator<Item = FungibleAllocation> + 'c {
        self.0.fungible_raw(OS_INFLATION, filter).unwrap()
    }
}

fn extract_global_single_val(
    mut global: impl Iterator<Item = StrictVal>,
) -> Result<Option<ContractId>, LinkError> {
    let Some(val) = global.next() else {
        return Ok(None);
    };
    if global.next().is_some() {
        return Err(LinkError::MultipleValues);
    }
    Ok(Some(ContractId::from_strict_val_unchecked(val)))
}

impl<S: ContractStateRead> LinkableSchemaWrapper<S> for BifaWrapper<S> {
    fn link_to(&self) -> Result<Option<ContractId>, LinkError> {
        extract_global_single_val(self.0.global("linkedToContract"))
    }

    fn link_from(&self) -> Result<Option<ContractId>, LinkError> {
        extract_global_single_val(self.0.global("linkedFromContract"))
    }
}

#[cfg(test)]
mod test {
    use rgbstd::schema::Occurrences;

    use crate::bifa::{bifa_schema, BIFA_SCHEMA_ID};
    use crate::{
        MS_CLAIM_AMOUNT, MS_CLAIM_CHAIN_ID, MS_CLAIM_CONTRACT, MS_CLAIM_EVENT_LOG_INDEX,
        MS_CLAIM_EVENT_TX_HASH, MS_CLAIM_NULLIFIER, MS_CLAIM_PROOF, OS_CLAIM_BATON, TS_CLAIM_MINT,
        TS_GRANT_BATON, TS_TRANSFER,
    };

    #[test]
    fn schema_id() {
        let schema_id = bifa_schema().schema_id();
        eprintln!("{:#04x?}", schema_id.to_byte_array());
        assert_eq!(BIFA_SCHEMA_ID, schema_id);
    }

    #[test]
    fn claim_mint_parallel_baton_invariants() {
        let schema = bifa_schema();
        let claim = &schema
            .transitions
            .get(&TS_CLAIM_MINT)
            .expect("claimMint transition must exist")
            .transition_schema;

        assert!(claim.metadata.contains(&MS_CLAIM_NULLIFIER));
        assert!(claim.metadata.contains(&MS_CLAIM_CHAIN_ID));
        assert!(claim.metadata.contains(&MS_CLAIM_CONTRACT));
        assert!(claim.metadata.contains(&MS_CLAIM_EVENT_TX_HASH));
        assert!(claim.metadata.contains(&MS_CLAIM_EVENT_LOG_INDEX));
        assert!(claim.metadata.contains(&MS_CLAIM_PROOF));
        assert!(claim.metadata.contains(&MS_CLAIM_AMOUNT));
        assert_eq!(schema.genesis.assignments.get(&OS_CLAIM_BATON), Some(&Occurrences::NoneOrMore));
        assert_eq!(claim.inputs.get(&OS_CLAIM_BATON), Some(&Occurrences::Once));
        assert_eq!(claim.assignments.get(&OS_CLAIM_BATON), Some(&Occurrences::Once));
    }

    #[test]
    fn transfer_cannot_move_claim_baton() {
        let schema = bifa_schema();
        let transfer = &schema
            .transitions
            .get(&TS_TRANSFER)
            .expect("transfer transition must exist")
            .transition_schema;

        assert!(!transfer.inputs.contains_key(&OS_CLAIM_BATON));
        assert!(!transfer.assignments.contains_key(&OS_CLAIM_BATON));
    }

    #[test]
    fn grant_baton_is_permissionless_and_parallel() {
        let schema = bifa_schema();
        let grant = &schema
            .transitions
            .get(&TS_GRANT_BATON)
            .expect("grantBaton transition must exist")
            .transition_schema;

        assert_eq!(grant.inputs.get(&OS_CLAIM_BATON), Some(&Occurrences::Once));
        assert_eq!(grant.assignments.get(&OS_CLAIM_BATON), Some(&Occurrences::OnceOrMore));
    }
}
