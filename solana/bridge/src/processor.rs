//! Program instruction processing logic
#![cfg(feature = "program")]

use std::borrow::Borrow;
use std::cell::RefCell;
use std::mem::size_of;
use std::slice::Iter;

use num_traits::AsPrimitive;
use primitive_types::U256;
use solana_sdk::clock::Clock;
use solana_sdk::hash::Hasher;
#[cfg(target_arch = "bpf")]
use solana_sdk::program::invoke_signed;
use solana_sdk::rent::Rent;
use solana_sdk::system_instruction::{create_account, SystemInstruction};
use solana_sdk::sysvar::Sysvar;
use solana_sdk::{
    account_info::next_account_info, account_info::AccountInfo, entrypoint::ProgramResult, info,
    instruction::Instruction, program_error::ProgramError, pubkey::Pubkey,
};
use spl_token::state::Mint;

use crate::error::Error;
use crate::instruction::BridgeInstruction::*;
use crate::instruction::{BridgeInstruction, TransferOutPayload, VAAData, CHAIN_ID_SOLANA};
use crate::instruction::{MAX_LEN_GUARDIAN_KEYS, MAX_VAA_SIZE};
use crate::state::*;
use crate::vaa::{BodyTransfer, BodyUpdateGuardianSet, VAABody, VAA};

/// Instruction processing logic
impl Bridge {
    /// Processes an [Instruction](enum.Instruction.html).
    pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], input: &[u8]) -> ProgramResult {
        let instruction = BridgeInstruction::deserialize(input)?;
        match instruction {
            Initialize(payload) => {
                info!("Instruction: Initialize");
                Self::process_initialize(
                    program_id,
                    accounts,
                    payload.len_guardians,
                    payload.initial_guardian,
                    payload.config,
                )
            }
            TransferOut(p) => {
                info!("Instruction: TransferOut");

                if p.asset.chain == CHAIN_ID_SOLANA {
                    Self::process_transfer_native_out(program_id, accounts, &p)
                } else {
                    Self::process_transfer_out(program_id, accounts, &p)
                }
            }
            PostVAA(vaa_body) => {
                info!("Instruction: PostVAA");
                let vaa = VAA::deserialize(&vaa_body)?;

                let hash = vaa.body_hash()?;

                Self::process_vaa(program_id, accounts, vaa_body, &vaa, &hash)
            }
            CreateWrapped(meta) => {
                info!("Instruction: CreateWrapped");
                Self::process_create_wrapped(program_id, accounts, &meta)
            }
            _ => panic!(""),
        }
    }

    /// Unpacks a token state from a bytes buffer while assuring that the state is initialized.
    pub fn process_initialize(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        len_guardians: u8,
        initial_guardian_key: [[u8; 20]; MAX_LEN_GUARDIAN_KEYS],
        config: BridgeConfig,
    ) -> ProgramResult {
        let account_info_iter = &mut accounts.iter();
        next_account_info(account_info_iter)?; // System program
        let clock_info = next_account_info(account_info_iter)?;
        let new_bridge_info = next_account_info(account_info_iter)?;
        let new_guardian_info = next_account_info(account_info_iter)?;
        let payer_info = next_account_info(account_info_iter)?;

        let clock = Clock::from_account_info(clock_info)?;

        // Create bridge account
        let bridge_seed = Bridge::derive_bridge_seeds();
        Bridge::check_and_create_account::<Bridge>(
            program_id,
            accounts,
            new_bridge_info.key,
            payer_info.key,
            program_id,
            &bridge_seed,
        )?;

        let mut new_account_data = new_bridge_info.data.borrow_mut();
        let mut bridge: &mut Bridge = Self::unpack_unchecked(&mut new_account_data)?;
        if bridge.is_initialized {
            return Err(Error::AlreadyExists.into());
        }

        // Create guardian set account
        let guardian_seed = Bridge::derive_guardian_set_seeds(new_bridge_info.key, 0);
        Bridge::check_and_create_account::<GuardianSet>(
            program_id,
            accounts,
            new_guardian_info.key,
            payer_info.key,
            program_id,
            &guardian_seed,
        )?;

        let mut new_guardian_data = new_guardian_info.data.borrow_mut();
        let mut guardian_info: &mut GuardianSet = Self::unpack_unchecked(&mut new_guardian_data)?;
        if guardian_info.is_initialized {
            return Err(Error::AlreadyExists.into());
        }

        if len_guardians > MAX_LEN_GUARDIAN_KEYS as u8 {
            return Err(ProgramError::InvalidInstructionData);
        }

        // Initialize bridge params
        bridge.is_initialized = true;
        bridge.guardian_set_index = 0;
        bridge.config = config;

        // Initialize the initial guardian set
        guardian_info.is_initialized = true;
        guardian_info.index = 0;
        guardian_info.creation_time = clock.unix_timestamp.as_();
        guardian_info.keys = initial_guardian_key;
        guardian_info.len_keys = len_guardians;

        Ok(())
    }

    /// Transfers a wrapped asset out
    pub fn process_transfer_out(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        t: &TransferOutPayload,
    ) -> ProgramResult {
        info!("wrapped transfer out");
        let account_info_iter = &mut accounts.iter();
        next_account_info(account_info_iter)?; // Bridge program
        next_account_info(account_info_iter)?; // System program
        next_account_info(account_info_iter)?; // Token program
        let sender_account_info = next_account_info(account_info_iter)?;
        let bridge_info = next_account_info(account_info_iter)?;
        let transfer_info = next_account_info(account_info_iter)?;
        let mint_info = next_account_info(account_info_iter)?;
        let payer_info = next_account_info(account_info_iter)?;

        let sender = Bridge::token_account_deserialize(sender_account_info)?;
        let bridge = Bridge::bridge_deserialize(bridge_info)?;

        // Does the token belong to the mint
        if sender.mint != *mint_info.key {
            return Err(Error::TokenMintMismatch.into());
        }

        // Check that the mint is actually a wrapped asset belonging to *this* bridge instance
        let expected_mint_address = Bridge::derive_wrapped_asset_id(
            program_id,
            bridge_info.key,
            t.asset.chain,
            t.asset.address,
        )?;
        if expected_mint_address != *mint_info.key {
            return Err(Error::InvalidDerivedAccount.into());
        }

        // Create transfer account
        let transfer_seed = Bridge::derive_transfer_id_seeds(
            bridge_info.key,
            t.asset.chain,
            t.asset.address,
            t.chain_id,
            t.target,
            sender_account_info.key.to_bytes(),
            t.nonce,
        );
        Bridge::check_and_create_account::<TransferOutProposal>(
            program_id,
            accounts,
            transfer_info.key,
            payer_info.key,
            program_id,
            &transfer_seed,
        )?;

        // Load transfer account
        let mut transfer_data = transfer_info.data.borrow_mut();
        let mut transfer: &mut TransferOutProposal = Self::unpack(&mut transfer_data)?;

        // Burn tokens
        Bridge::wrapped_burn(
            program_id,
            accounts,
            &bridge.config.token_program,
            sender_account_info.key,
            t.amount,
        )?;

        // Initialize transfer
        transfer.is_initialized = true;
        transfer.nonce = t.nonce;
        transfer.source_address = sender_account_info.key.to_bytes();
        transfer.foreign_address = t.target;
        transfer.amount = t.amount;
        transfer.to_chain_id = t.chain_id;
        transfer.asset = t.asset;

        Ok(())
    }

    /// Creates a new wrapped asset
    pub fn process_create_wrapped(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        a: &AssetMeta,
    ) -> ProgramResult {
        info!("create wrapped");
        let account_info_iter = &mut accounts.iter();
        next_account_info(account_info_iter)?; // Bridge program
        next_account_info(account_info_iter)?; // System program
        next_account_info(account_info_iter)?; // Token program
        let bridge_info = next_account_info(account_info_iter)?;
        let payer_info = next_account_info(account_info_iter)?;
        let mint_info = next_account_info(account_info_iter)?;
        let wrapped_meta_info = next_account_info(account_info_iter)?;

        let bridge = Bridge::bridge_deserialize(bridge_info)?;

        if a.chain == CHAIN_ID_SOLANA {
            return Err(Error::CannotWrapNative.into());
        }

        // Create wrapped mint
        Self::create_wrapped_mint(
            program_id,
            accounts,
            &bridge.config.token_program,
            mint_info.key,
            bridge_info.key,
            payer_info.key,
            &a,
        )?;

        // Check and create wrapped asset meta to allow reverse resolution of info
        let wrapped_meta_seeds = Bridge::derive_wrapped_meta_seeds(bridge_info.key, mint_info.key);
        Bridge::check_and_create_account::<WrappedAssetMeta>(
            program_id,
            accounts,
            wrapped_meta_info.key,
            payer_info.key,
            program_id,
            &wrapped_meta_seeds,
        )?;

        let mut wrapped_meta_data = wrapped_meta_info.data.borrow_mut();
        let wrapped_meta: &mut WrappedAssetMeta = Bridge::unpack_unchecked(&mut wrapped_meta_data)?;

        wrapped_meta.is_initialized = true;
        wrapped_meta.address = a.address;
        wrapped_meta.chain = a.chain;

        Ok(())
    }

    /// Transfers a native token to a foreign chain
    pub fn process_transfer_native_out(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        t: &TransferOutPayload,
    ) -> ProgramResult {
        info!("native transfer out");
        let account_info_iter = &mut accounts.iter();
        next_account_info(account_info_iter)?; // Bridge program
        next_account_info(account_info_iter)?; // System program
        next_account_info(account_info_iter)?; // Token program
        let sender_account_info = next_account_info(account_info_iter)?;
        let bridge_info = next_account_info(account_info_iter)?;
        let transfer_info = next_account_info(account_info_iter)?;
        let mint_info = next_account_info(account_info_iter)?;
        let payer_info = next_account_info(account_info_iter)?;
        let custody_info = next_account_info(account_info_iter)?;

        let sender = Bridge::token_account_deserialize(sender_account_info)?;
        let bridge = Bridge::bridge_deserialize(bridge_info)?;

        // Does the token belong to the mint
        if sender.mint != *mint_info.key {
            return Err(Error::TokenMintMismatch.into());
        }

        // Create transfer account
        let transfer_seed = Bridge::derive_transfer_id_seeds(
            bridge_info.key,
            t.asset.chain,
            t.asset.address,
            t.chain_id,
            t.target,
            sender_account_info.key.to_bytes(),
            t.nonce,
        );
        Bridge::check_and_create_account::<TransferOutProposal>(
            program_id,
            accounts,
            transfer_info.key,
            payer_info.key,
            program_id,
            &transfer_seed,
        )?;

        // Load transfer account
        let mut transfer_data = transfer_info.data.borrow_mut();
        let mut transfer: &mut TransferOutProposal = Self::unpack_unchecked(&mut transfer_data)?;

        // Check that custody account was derived correctly
        let expected_custody_id =
            Bridge::derive_custody_id(program_id, bridge_info.key, mint_info.key)?;
        if expected_custody_id != *custody_info.key {
            return Err(Error::InvalidDerivedAccount.into());
        }

        // Create the account if it does not exist
        if custody_info.data_is_empty() {
            Bridge::create_custody_account(
                program_id,
                accounts,
                &bridge.config.token_program,
                bridge_info.key,
                custody_info.key,
                mint_info.key,
                payer_info.key,
            )?;
        }

        let bridge_authority = Self::derive_bridge_id(program_id)?;

        // Check that the custody token account is owned by the derived key
        let custody = Self::token_account_deserialize(custody_info)?;
        if custody.owner != bridge_authority {
            return Err(Error::WrongTokenAccountOwner.into());
        }

        info!("transferring");
        // Transfer tokens to custody - This also checks that custody mint = mint
        Bridge::token_transfer_caller(
            program_id,
            accounts,
            &bridge.config.token_program,
            sender_account_info.key,
            custody_info.key,
            &bridge_authority,
            t.amount,
        )?;

        // Initialize proposal
        transfer.is_initialized = true;
        transfer.amount = t.amount;
        transfer.to_chain_id = t.chain_id;
        transfer.source_address = sender_account_info.key.to_bytes();
        transfer.foreign_address = t.target;
        transfer.nonce = t.nonce;

        // Don't use the user-given data as we don't check mint = AssetMeta.address
        transfer.asset = AssetMeta {
            chain: CHAIN_ID_SOLANA,
            address: mint_info.key.to_bytes(),
        };

        Ok(())
    }

    /// Processes a VAA
    pub fn process_vaa(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        vaa_data: VAAData,
        vaa: &VAA,
        hash: &[u8; 32],
    ) -> ProgramResult {
        let account_info_iter = &mut accounts.iter();

        // Load VAA processing default accounts
        next_account_info(account_info_iter)?; // Bridge program
        next_account_info(account_info_iter)?; // System program
        let clock_info = next_account_info(account_info_iter)?;
        let bridge_info = next_account_info(account_info_iter)?;
        let guardian_set_info = next_account_info(account_info_iter)?;
        let claim_info = next_account_info(account_info_iter)?;
        let payer_info = next_account_info(account_info_iter)?;

        let mut bridge = Bridge::bridge_deserialize(bridge_info)?;
        let clock = Clock::from_account_info(clock_info)?;
        let mut guardian_set = Bridge::guardian_set_deserialize(guardian_set_info)?;

        // Check that the guardian set is valid
        let expected_guardian_set =
            Bridge::derive_guardian_set_id(program_id, bridge_info.key, vaa.guardian_set_index)?;
        if expected_guardian_set != *guardian_set_info.key {
            return Err(Error::InvalidDerivedAccount.into());
        }

        // Check and create claim
        let claim_seeds = Bridge::derive_claim_seeds(bridge_info.key, hash);
        Bridge::check_and_create_account::<ClaimedVAA>(
            program_id,
            accounts,
            claim_info.key,
            payer_info.key,
            program_id,
            &claim_seeds,
        )?;

        // Check that the guardian set is still active
        if (guardian_set.expiration_time as i64) > clock.unix_timestamp {
            return Err(Error::GuardianSetExpired.into());
        }

        // Check that the VAA is still valid
        if (guardian_set.expiration_time as i64) + (bridge.config.vaa_expiration_time as i64)
            > clock.unix_timestamp
        {
            return Err(Error::VAAExpired.into());
        }

        // Verify VAA signature
        if !vaa.verify(&guardian_set.keys[..guardian_set.len_keys as usize]) {
            return Err(Error::InvalidVAASignature.into());
        }

        let payload = vaa.payload.as_ref().ok_or(Error::InvalidVAAAction)?;
        match payload {
            VAABody::UpdateGuardianSet(v) => Self::process_vaa_set_update(
                program_id,
                accounts,
                account_info_iter,
                &clock,
                bridge_info,
                payer_info,
                &mut bridge,
                &mut guardian_set,
                &v,
            ),
            VAABody::Transfer(v) => {
                if v.source_chain == CHAIN_ID_SOLANA {
                    Self::process_vaa_transfer_post(
                        program_id,
                        account_info_iter,
                        bridge_info,
                        vaa,
                        &v,
                        vaa_data,
                    )
                } else {
                    Self::process_vaa_transfer(
                        program_id,
                        accounts,
                        account_info_iter,
                        bridge_info,
                        payer_info,
                        &mut bridge,
                        &v,
                    )
                }
            }
        }?;

        // Load claim account
        let mut claim_data = claim_info.data.borrow_mut();
        let claim: &mut ClaimedVAA = Bridge::unpack_unchecked(&mut claim_data)?;
        if claim.is_initialized {
            return Err(Error::VAAClaimed.into());
        }

        // Set claimed
        claim.is_initialized = true;
        claim.vaa_time = clock.unix_timestamp as u32;

        Ok(())
    }

    /// Processes a Guardian set update
    pub fn process_vaa_set_update(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        account_info_iter: &mut Iter<AccountInfo>,
        clock: &Clock,
        bridge_info: &AccountInfo,
        payer_info: &AccountInfo,
        bridge: &mut Bridge,
        old_guardian_set: &mut GuardianSet,
        b: &BodyUpdateGuardianSet,
    ) -> ProgramResult {
        let new_guardian_info = next_account_info(account_info_iter)?;

        // TODO this could deadlock the bridge if an update is performed with an invalid key
        // The new guardian set must be signed by the current one
        if bridge.guardian_set_index != old_guardian_set.index {
            return Err(Error::OldGuardianSet.into());
        }

        // The new guardian set must have an index > current
        // We don't check +1 because we trust the set to not set something close to max(u32)
        if bridge.guardian_set_index >= b.new_index {
            return Err(Error::GuardianIndexNotIncreasing.into());
        }

        // Set the exirity on the old guardian set
        // The guardian set will expire once all currently issues vaas have expired
        old_guardian_set.expiration_time =
            (clock.unix_timestamp as u32) + bridge.config.vaa_expiration_time;

        // Check whether the new guardian set was derived correctly
        let guardian_seed = Bridge::derive_guardian_set_seeds(bridge_info.key, b.new_index);
        Bridge::check_and_create_account::<GuardianSet>(
            program_id,
            accounts,
            new_guardian_info.key,
            payer_info.key,
            program_id,
            &guardian_seed,
        )?;

        let mut guardian_set_new_data = new_guardian_info.data.borrow_mut();
        let guardian_set_new: &mut GuardianSet =
            Bridge::unpack_unchecked(&mut guardian_set_new_data)?;

        // The new guardian set must not exist
        if guardian_set_new.is_initialized {
            return Err(Error::AlreadyExists.into());
        }

        if b.new_keys.len() > MAX_LEN_GUARDIAN_KEYS {
            return Err(Error::InvalidVAAFormat.into());
        }

        // Set values on the new guardian set
        guardian_set_new.is_initialized = true;
        guardian_set_new.index = b.new_index;
        let mut new_guardians = [[0u8; 20]; MAX_LEN_GUARDIAN_KEYS];
        for n in 0..b.new_keys.len() {
            new_guardians[n] = b.new_keys[n]
        }
        guardian_set_new.keys = new_guardians;
        guardian_set_new.len_keys = b.new_keys.len() as u8;
        guardian_set_new.creation_time = clock.unix_timestamp as u32;

        // Update the bridge guardian set id
        bridge.guardian_set_index = b.new_index;

        Ok(())
    }

    /// Processes a VAA transfer in
    pub fn process_vaa_transfer(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        account_info_iter: &mut Iter<AccountInfo>,
        bridge_info: &AccountInfo,
        payer_info: &AccountInfo,
        bridge: &mut Bridge,
        b: &BodyTransfer,
    ) -> ProgramResult {
        next_account_info(account_info_iter)?; // Token program
        let mint_info = next_account_info(account_info_iter)?;
        let destination_info = next_account_info(account_info_iter)?;

        let destination = Self::token_account_deserialize(destination_info)?;
        if destination.mint != *mint_info.key {
            return Err(Error::TokenMintMismatch.into());
        }

        if b.asset.chain == CHAIN_ID_SOLANA {
            let custody_info = next_account_info(account_info_iter)?;
            let expected_custody_id =
                Bridge::derive_custody_id(program_id, bridge_info.key, mint_info.key)?;
            if expected_custody_id != *custody_info.key {
                return Err(Error::InvalidDerivedAccount.into());
            }

            // Native Solana asset, transfer from custody
            Bridge::token_transfer_custody(
                program_id,
                accounts,
                &bridge.config.token_program,
                custody_info.key,
                destination_info.key,
                b.amount,
            )?;
        } else {
            // Foreign chain asset, mint wrapped asset
            let expected_mint_address = Bridge::derive_wrapped_asset_id(
                program_id,
                bridge_info.key,
                b.asset.chain,
                b.asset.address,
            )?;
            if expected_mint_address != *mint_info.key {
                return Err(Error::InvalidDerivedAccount.into());
            }

            // This automatically asserts that the mint was created by this account by using
            // derivated keys
            Bridge::wrapped_mint_to(
                program_id,
                accounts,
                &bridge.config.token_program,
                mint_info.key,
                destination_info.key,
                b.amount,
            )?;
        }

        Ok(())
    }

    /// Processes a VAA post for data availability (for Solana -> foreign transfers)
    pub fn process_vaa_transfer_post(
        program_id: &Pubkey,
        account_info_iter: &mut Iter<AccountInfo>,
        bridge_info: &AccountInfo,
        vaa: &VAA,
        b: &BodyTransfer,
        vaa_data: VAAData,
    ) -> ProgramResult {
        let proposal_info = next_account_info(account_info_iter)?;

        // Check whether the proposal was derived correctly
        let expected_proposal = Bridge::derive_transfer_id(
            program_id,
            bridge_info.key,
            b.asset.chain,
            b.asset.address,
            b.target_chain,
            b.target_address,
            b.source_address,
            b.nonce,
        )?;
        if expected_proposal != *proposal_info.key {
            return Err(Error::InvalidDerivedAccount.into());
        }

        let mut transfer_data = proposal_info.data.borrow_mut();
        let mut proposal: &mut TransferOutProposal = Self::unpack(&mut transfer_data)?;
        if !proposal.matches_vaa(b) {
            return Err(Error::VAAProposalMismatch.into());
        }
        if proposal.vaa_time != 0 {
            return Err(Error::VAAAlreadySubmitted.into());
        }
        if vaa_data.len() > MAX_VAA_SIZE {
            return Err(Error::VAATooLong.into());
        }

        // Set vaa
        for i in 0..vaa_data.len() {
            proposal.vaa[i] = vaa_data[i]
        }
        // Stop byte
        proposal.vaa[vaa_data.len()] = 0xff;
        proposal.vaa_time = vaa.timestamp;

        Ok(())
    }
}

/// Implementation of actions
impl Bridge {
    /// Burn a wrapped asset from account
    pub fn wrapped_burn(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        token_program_id: &Pubkey,
        token_account: &Pubkey,
        amount: U256,
    ) -> Result<(), ProgramError> {
        let ix = spl_token::instruction::burn(
            token_program_id,
            token_account,
            &Self::derive_bridge_id(program_id)?,
            &[],
            amount.as_u64(),
        )?;
        Self::invoke_as_bridge(program_id, &ix, accounts)
    }

    /// Mint a wrapped asset to account
    pub fn wrapped_mint_to(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        token_program_id: &Pubkey,
        mint: &Pubkey,
        destination: &Pubkey,
        amount: U256,
    ) -> Result<(), ProgramError> {
        let ix = spl_token::instruction::mint_to(
            token_program_id,
            mint,
            destination,
            &Self::derive_bridge_id(program_id)?,
            &[],
            amount.as_u64(),
        )?;
        Self::invoke_as_bridge(program_id, &ix, accounts)
    }

    /// Transfer tokens from a caller
    pub fn token_transfer_caller(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        token_program_id: &Pubkey,
        source: &Pubkey,
        destination: &Pubkey,
        authority: &Pubkey,
        amount: U256,
    ) -> Result<(), ProgramError> {
        let ix = spl_token::instruction::transfer(
            token_program_id,
            source,
            destination,
            authority,
            &[],
            amount.as_u64(),
        )?;
        Self::invoke_as_bridge(program_id, &ix, accounts)
    }

    /// Transfer tokens from a custody account
    pub fn token_transfer_custody(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        token_program_id: &Pubkey,
        source: &Pubkey,
        destination: &Pubkey,
        amount: U256,
    ) -> Result<(), ProgramError> {
        let ix = spl_token::instruction::transfer(
            token_program_id,
            source,
            destination,
            &Self::derive_bridge_id(program_id)?,
            &[],
            amount.as_u64(),
        )?;
        Self::invoke_as_bridge(program_id, &ix, accounts)
    }

    /// Create a new account
    pub fn create_custody_account(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        token_program: &Pubkey,
        bridge: &Pubkey,
        account: &Pubkey,
        mint: &Pubkey,
        payer: &Pubkey,
    ) -> Result<(), ProgramError> {
        Self::check_and_create_account::<spl_token::state::Account>(
            program_id,
            accounts,
            account,
            payer,
            token_program,
            &Self::derive_custody_seeds(bridge, mint),
        )?;
        info!("bababu");
        info!(token_program.to_string().as_str());
        let ix = spl_token::instruction::initialize_account(
            token_program,
            account,
            mint,
            &Self::derive_bridge_id(program_id)?,
        )?;
        invoke_signed(&ix, accounts, &[])
    }

    /// Create a mint for a wrapped asset
    pub fn create_wrapped_mint(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        token_program: &Pubkey,
        mint: &Pubkey,
        bridge: &Pubkey,
        payer: &Pubkey,
        asset: &AssetMeta,
    ) -> Result<(), ProgramError> {
        Self::check_and_create_account::<Mint>(
            program_id,
            accounts,
            mint,
            payer,
            token_program,
            &Self::derive_wrapped_asset_seeds(bridge, asset.chain, asset.address),
        )?;
        let ix = spl_token::instruction::initialize_mint(
            token_program,
            mint,
            None,
            Some(&Self::derive_bridge_id(program_id)?),
            0,
            18,
        )?;
        invoke_signed(&ix, accounts, &[])
    }

    pub fn invoke_as_bridge<'a>(
        program_id: &Pubkey,
        instruction: &Instruction,
        account_infos: &[AccountInfo<'a>],
    ) -> ProgramResult {
        let (_, seeds) =
            Self::find_program_address(&vec!["bridge".as_bytes().to_vec()], program_id);
        Self::invoke_vec_seed(program_id, instruction, account_infos, &seeds)
    }

    pub fn invoke_vec_seed<'a>(
        program_id: &Pubkey,
        instruction: &Instruction,
        account_infos: &[AccountInfo<'a>],
        seeds: &Vec<Vec<u8>>,
    ) -> ProgramResult {
        let s: Vec<_> = seeds.iter().map(|item| item.as_slice()).collect();
        invoke_signed(instruction, account_infos, &[s.as_slice()])
    }

    /// Check that a key was derived correctly and create account
    pub fn check_and_create_account<T: Sized>(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        new_account: &Pubkey,
        payer: &Pubkey,
        owner: &Pubkey,
        seeds: &Vec<Vec<u8>>,
    ) -> Result<Vec<Vec<u8>>, ProgramError> {
        info!("deriving key");
        let (expected_key, full_seeds) = Bridge::derive_key(program_id, seeds)?;
        if expected_key != *new_account {
            return Err(Error::InvalidDerivedAccount.into());
        }

        info!("deploying contract");
        Self::create_account_raw::<T>(
            program_id,
            accounts,
            new_account,
            payer,
            owner,
            &full_seeds,
        )?;

        Ok(full_seeds)
    }

    /// Create a new account
    fn create_account_raw<T: Sized>(
        program_id: &Pubkey,
        accounts: &[AccountInfo],
        new_account: &Pubkey,
        payer: &Pubkey,
        owner: &Pubkey,
        seeds: &Vec<Vec<u8>>,
    ) -> Result<(), ProgramError> {
        let size = size_of::<T>();
        let ix = create_account(
            payer,
            new_account,
            Rent::default().minimum_balance(size as usize),
            size as u64,
            owner,
        );
        let s: Vec<_> = seeds.iter().map(|item| item.as_slice()).collect();
        invoke_signed(&ix, accounts, &[s.as_slice()])
    }
}
