//! wrapping of mpt-circuit
use crate::{
    bytecode_circuit::bytecode_unroller::HASHBLOCK_BYTES_IN_FIELD,
    table::PoseidonTable,
    util::{Challenges, SubCircuit, SubCircuitConfig},
    witness,
};
use std::{fs::File, io::Write};
use bus_mapping::state_db::CodeDB;
use eth_types::Field;
use halo2_proofs::{
    circuit::{Layouter, SimpleFloorPlanner, Value},
    halo2curves::bn256::Fr,
    plonk::{Circuit, ConstraintSystem, Error},
};
use hash_circuit::{
    hash::{Hashable, PoseidonHashChip, PoseidonHashConfig, PoseidonHashTable},
    Bn256Fr,
};
use itertools::Itertools;
use mpt_zktrie::mpt_circuits::{
    gadgets::mpt_update::hash_traces, serde::SMTTrace, types::Proof, MPTProofType,
};

/// re-wrapping for mpt circuit
#[derive(Default, Clone, Debug)]
pub struct PoseidonCircuit<F: Field>(pub(crate) PoseidonHashTable<F>, usize);

/// Circuit configuration argument ts
pub struct PoseidonCircuitConfigArgs {
    /// PoseidonTable
    pub poseidon_table: PoseidonTable,
}

/// re-wrapping for poseidon config
#[derive(Debug, Clone)]
pub struct PoseidonCircuitConfig<F: Field>(pub(crate) PoseidonHashConfig<F>);

const HASH_BLOCK_STEP_SIZE: usize = HASHBLOCK_BYTES_IN_FIELD * PoseidonTable::INPUT_WIDTH;

impl<F: Field> SubCircuitConfig<F> for PoseidonCircuitConfig<F> {
    type ConfigArgs = PoseidonCircuitConfigArgs;

    fn new(
        meta: &mut ConstraintSystem<F>,
        Self::ConfigArgs { poseidon_table }: Self::ConfigArgs,
    ) -> Self {
        let poseidon_table = (
            poseidon_table.q_enable,
            [
                poseidon_table.hash_id,
                poseidon_table.input0,
                poseidon_table.input1,
                poseidon_table.control,
                poseidon_table.heading_mark,
            ],
        );
        let conf = PoseidonHashConfig::configure_sub(meta, poseidon_table, HASH_BLOCK_STEP_SIZE);
        Self(conf)
    }
}

#[cfg(any(feature = "test", test))]
impl<F: Field> SubCircuit<F> for PoseidonCircuit<F> {
    type Config = PoseidonCircuitConfig<F>;

    fn new_from_block(block: &witness::Block<F>) -> Self {
        let max_hashes = block.circuits_params.max_evm_rows / F::hash_block_size();
        #[allow(unused_mut)]
        let mut poseidon_table_data: PoseidonHashTable<F> = PoseidonHashTable::default();
        // without any feature we just synthesis an empty poseidon circuit
        #[cfg(feature = "zktrie")]
        {
            // TODO: check here to avoid duplicate calculation
            let traces: Vec<(MPTProofType, SMTTrace)> = block
                .mpt_updates
                .proof_types
                .iter()
                .cloned()
                .zip_eq(block.mpt_updates.smt_traces.iter().cloned())
                .collect();

                let mut dump = File::create("dump.json").unwrap();
                dump.write_all(serde_json::to_string_pretty(&traces).unwrap().as_bytes()).unwrap();

            let proofs: Vec<Proof> = traces.into_iter().map(Proof::from).collect();
            let triples: Vec<(Fr, Fr, Fr)> = hash_traces(&proofs);
            let triples: Vec<(F, F, F)> = triples
                .into_iter()
                .map(|(a, b, c)| (a.into(), b.into(), c.into()))
                .collect();
            for elems in &triples {
                if elems.2.is_zero_vartime() {
                    log::info!("zero hash {:?}", elems);
                }
            }
            poseidon_table_data.constant_inputs_with_check(&triples);
        }
        #[cfg(feature = "poseidon-codehash")]
        {
            use crate::bytecode_circuit::bytecode_unroller::unroll_to_hash_input_default;
            for bytecode in block.bytecodes.values() {
                // must skip empty bytecode
                if !bytecode.bytes.is_empty() {
                    let unrolled_inputs =
                        unroll_to_hash_input_default::<F>(bytecode.bytes.iter().copied());
                    poseidon_table_data.stream_inputs(
                        &unrolled_inputs,
                        bytecode.bytes.len() as u64,
                        HASH_BLOCK_STEP_SIZE,
                    );
                }
            }
        }

        Self(poseidon_table_data, max_hashes)
    }

    fn min_num_rows_block(block: &witness::Block<F>) -> (usize, usize) {
        let acc = 0;
        #[cfg(feature = "zktrie")]
        let acc = {
            let mut cnt = acc;
            let traces: Vec<_> = block
                .mpt_updates
                .proof_types
                .iter()
                .cloned()
                .zip_eq(block.mpt_updates.smt_traces.iter().cloned())
                .collect();

            let mut dump = File::create("dump.json").unwrap();
            dump.write_all(serde_json::to_string_pretty(&traces).unwrap().as_bytes()).unwrap();

            let proofs: Vec<Proof> = traces.into_iter().map(Proof::from).collect();
            cnt += hash_traces(&proofs).len();
            cnt
        };
        #[cfg(feature = "poseidon-codehash")]
        let acc = {
            let mut cnt = acc;
            use crate::bytecode_circuit::bytecode_unroller::unroll_to_hash_input_default;
            for bytecode in block.bytecodes.values() {
                cnt += unroll_to_hash_input_default::<F>(bytecode.bytes.iter().copied()).len();
            }
            cnt
        };
        let acc = acc * F::hash_block_size();
        (acc, block.circuits_params.max_evm_rows.max(acc))
    }

    /// Make the assignments to the MptCircuit, notice it fill mpt table
    /// but not fill hash table
    fn synthesize_sub(
        &self,
        config: &Self::Config,
        _challenges: &Challenges<Value<F>>,
        layouter: &mut impl Layouter<F>,
    ) -> Result<(), Error> {
        // for single codehash we sitll use keccak256(nil)
        use eth_types::{ToScalar, ToWord};
        // Note the Option(nil_hash) in construct has different meanings as the returning of
        // `to_scalar` so we should not use the returning option here
        let empty_hash = CodeDB::empty_code_hash().to_word().to_scalar().unwrap();

        let chip = PoseidonHashChip::<_, HASH_BLOCK_STEP_SIZE>::construct(
            config.0.clone(),
            &self.0,
            self.1,
            false,
            Some(empty_hash),
        );

        chip.load(layouter)
    }

    /// powers of randomness for instance columns
    fn instance(&self) -> Vec<Vec<F>> {
        vec![]
    }
}

#[cfg(any(feature = "test", test))]
impl<F: Field + Hashable> Circuit<F> for PoseidonCircuit<F> {
    type Config = (PoseidonCircuitConfig<F>, Challenges);
    type FloorPlanner = SimpleFloorPlanner;

    fn without_witnesses(&self) -> Self {
        Self(Default::default(), self.1)
    }

    fn configure(meta: &mut ConstraintSystem<F>) -> Self::Config {
        let challenges = Challenges::construct(meta);
        let poseidon_table = PoseidonTable::construct(meta);

        let config =
            { PoseidonCircuitConfig::new(meta, PoseidonCircuitConfigArgs { poseidon_table }) };

        (config, challenges)
    }

    fn synthesize(
        &self,
        (config, challenges): Self::Config,
        mut layouter: impl Layouter<F>,
    ) -> Result<(), Error> {
        let challenges = challenges.values(&layouter);
        self.synthesize_sub(&config, &challenges, &mut layouter)
    }
}
