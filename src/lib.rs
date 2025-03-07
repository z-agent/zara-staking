use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};
use anchor_spl::associated_token::AssociatedToken;

declare_id!("8mdqFh7gtdZb2jpUtzoQRFwmjbGE3KAfzq3TqEJxknMt");

// Constants with correct denominations (1 ZARA = 1_000_000 lamports, 6 decimals)
pub const BASIC_TIER_AMOUNT: u64    = 5_000_000_000;     // 5,000 ZARA
pub const PREMIUM_TIER_AMOUNT: u64  = 50_000_000_000;    // 50,000 ZARA
pub const ELITE_TIER_AMOUNT: u64    = 250_000_000_000;   // 250,000 ZARA

// APR constants remain the same as they're in basis points
pub const BASIC_APR: u64    = 400;  // 4% base APR
pub const PREMIUM_APR: u64  = 600;  // 6% base APR
pub const ELITE_APR: u64    = 800;  // 8% base APR

// Lock periods remain the same (in seconds)
pub const BASIC_LOCK: i64   = 3 * 24 * 60 * 60;  // 3 days
pub const PREMIUM_LOCK: i64 = 7 * 24 * 60 * 60;  // 7 days
pub const ELITE_LOCK: i64   = 14 * 24 * 60 * 60; // 14 days

// Pool constants (using 6 decimals)
pub const INITIAL_REWARD_POOL: u64 = 5_000_000_000_000;     // 5M ZARA initial reward pool
pub const MAX_ANNUAL_REWARDS: u64  = 1_000_000_000_000;     // 1M ZARA max annual rewards

// Add ZARA mint pubkey constant
// #[cfg(feature = "devnet")]
// pub const ZARA_MINT_PUBKEY: Pubkey = Pubkey::new_from_array([
//     // Devnet mint: Arf7WHpC66mFUFMWhcGdFF5c9zcWdDk62AfVnLLZ5dGQ
//     65, 114, 102, 55, 87, 72, 112, 67, 54, 54, 109, 70, 85, 70, 77, 87,
//     104, 99, 71, 100, 70, 70, 53, 99, 57, 122, 99, 87, 100, 68, 107, 54,
//     50, 65, 102, 86, 110, 76, 76, 90, 53, 100, 71, 81
// ]);

// #[cfg(not(feature = "devnet"))]
// pub const ZARA_MINT_PUBKEY: Pubkey = Pubkey::new_from_array([
//     // Mainnet mint: 73UdJevxaNKXARgkvPHQGKuv8HCZARszuKW2LTL3pump
//     55, 51, 85, 100, 74, 101, 118, 120, 97, 78, 75, 88, 65, 82, 103, 107,
//     118, 80, 72, 81, 71, 75, 117, 118, 56, 72, 67, 90, 65, 82, 115, 122
// ]);

#[program]
pub mod zara_staking {
    use super::*;

    pub fn initialize_pool(
        ctx: Context<InitializePool>,
        reward_pool_amount: u64,
    ) -> Result<()> {
        msg!("Initializing pool with reward amount: {}", reward_pool_amount);
        msg!("Max allowed amount: {}", INITIAL_REWARD_POOL);

        // Validate reward pool amount
        require!(
            reward_pool_amount <= INITIAL_REWARD_POOL,
            StakingError::ExcessiveRewardPool
        );

        let pool = &mut ctx.accounts.pool;
        pool.authority = ctx.accounts.authority.key();
        pool.zara_mint = ctx.accounts.zara_mint.key();
        pool.total_staked = 0;
        pool.staker_count = 0;
        pool.paused = false;
        pool.reward_pool_amount = reward_pool_amount;
        pool.next_position_id = 1;  // Start from 1, no assumptions

        msg!("Pool initialized successfully!");
        msg!("Authority: {}", pool.authority);
        msg!("ZARA mint: {}", pool.zara_mint);

        emit!(PoolInitializedEvent {
            authority: pool.authority,
            reward_pool: reward_pool_amount,
            timestamp: Clock::get()?.unix_timestamp,
        });

        Ok(())
    }

    pub fn stake(ctx: Context<Stake>, amount: u64) -> Result<()> {
        ctx.accounts.process(amount)
    }

    pub fn unstake(ctx: Context<Unstake>) -> Result<()> {
        let clock = Clock::get()?;
        let staker = &ctx.accounts.staker;

        // Verify minimum stake duration
        require!(
            clock.unix_timestamp >= staker.stake_end_time,
            StakingError::MinStakeDurationNotMet
        );

        // Validate stake vault has enough balance
        require!(
            ctx.accounts.stake_vault.amount >= staker.staked_amount,
            StakingError::InsufficientStakeVault
        );

        // Calculate rewards
        let rewards = calculate_rewards(
            staker.staked_amount,
            staker.last_claim_time,
            clock.unix_timestamp,
            staker.apr,
        )?;

        // Validate reward vault has enough balance if rewards > 0
        if rewards > 0 {
            require!(
                ctx.accounts.reward_vault.amount >= rewards,
                StakingError::InsufficientRewardPool
            );
        }

        // Transfer staked tokens back
        let seeds = &[b"pool_v3" as &[u8], &[ctx.bumps.pool]];
        let signer = &[&seeds[..]];

        let cpi_program = ctx.accounts.token_program.to_account_info();

        // Transfer staked tokens
        {
            let cpi_accounts = token::Transfer {
                from: ctx.accounts.stake_vault.to_account_info(),
                to: ctx.accounts.staker_token_account.to_account_info(),
                authority: ctx.accounts.pool.to_account_info(),
            };
            let cpi_ctx = CpiContext::new_with_signer(cpi_program.clone(), cpi_accounts, signer);
            token::transfer(cpi_ctx, staker.staked_amount)?;
        }

        // Transfer rewards if any
        if rewards > 0 {
            let cpi_accounts = token::Transfer {
                from: ctx.accounts.reward_vault.to_account_info(),
                to: ctx.accounts.staker_token_account.to_account_info(),
                authority: ctx.accounts.pool.to_account_info(),
            };
            let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);
            token::transfer(cpi_ctx, rewards)?;
        }

        // Update pool info with checked math
        let pool = &mut ctx.accounts.pool;
        pool.total_staked = pool.total_staked.checked_sub(staker.staked_amount).unwrap();
        pool.staker_count = pool.staker_count.checked_sub(1).unwrap();

        // Emit unstake event
        emit!(UnstakeEvent {
            owner: ctx.accounts.owner.key(),
            position_id: staker.position_id,
            amount: staker.staked_amount,
            rewards,
            timestamp: clock.unix_timestamp,
        });

        Ok(())
    }

    pub fn emergency_unstake(ctx: Context<Unstake>) -> Result<()> {
        require!(ctx.accounts.pool.paused, StakingError::PoolNotPaused);
        
        let staker = &ctx.accounts.staker;

        // Validate stake vault has enough balance
        require!(
            ctx.accounts.stake_vault.amount >= staker.staked_amount,
            StakingError::InsufficientStakeVault
        );

        // Transfer only staked tokens back (no rewards)
        let seeds = &[b"pool_v3" as &[u8], &[ctx.bumps.pool]];
        let signer = &[&seeds[..]];

        let cpi_accounts = token::Transfer {
            from: ctx.accounts.stake_vault.to_account_info(),
            to: ctx.accounts.staker_token_account.to_account_info(),
            authority: ctx.accounts.pool.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signer
        );
        token::transfer(cpi_ctx, staker.staked_amount)?;

        // Update pool info with checked math
        let pool = &mut ctx.accounts.pool;
        pool.total_staked = pool.total_staked.checked_sub(staker.staked_amount).unwrap();
        pool.staker_count = pool.staker_count.checked_sub(1).unwrap();

        // Emit emergency unstake event
        emit!(EmergencyUnstakeEvent {
            owner: ctx.accounts.owner.key(),
            position_id: staker.position_id,
            amount: staker.staked_amount,
            timestamp: Clock::get()?.unix_timestamp,
        });

        Ok(())
    }

    pub fn claim_rewards(ctx: Context<ClaimRewards>) -> Result<()> {
        require!(!ctx.accounts.pool.paused, StakingError::PoolPaused);
        
        let clock = Clock::get()?;
        let staker = &mut ctx.accounts.staker;

        // Calculate rewards
        let rewards = calculate_rewards(
            staker.staked_amount,
            staker.last_claim_time,
            clock.unix_timestamp,
            staker.apr,
        )?;

        if rewards > 0 {
            // Validate reward vault has enough balance
            require!(
                ctx.accounts.reward_vault.amount >= rewards,
                StakingError::InsufficientRewardPool
            );

            // Validate against annual reward limit
            let annual_rewards = calculate_annual_rewards(staker.staked_amount, staker.apr)?;
            require!(
                rewards <= annual_rewards,
                StakingError::AnnualRewardLimitExceeded
            );

            // Transfer rewards
            let seeds = &[b"pool_v3" as &[u8], &[ctx.bumps.pool]];
            let signer = &[&seeds[..]];

            let cpi_accounts = token::Transfer {
                from: ctx.accounts.reward_vault.to_account_info(),
                to: ctx.accounts.staker_token_account.to_account_info(),
                authority: ctx.accounts.pool.to_account_info(),
            };
            let cpi_program = ctx.accounts.token_program.to_account_info();
            let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer);
            token::transfer(cpi_ctx, rewards)?;

            // Update staker info with checked math
            staker.last_claim_time = clock.unix_timestamp;
            staker.reward_debt = staker.reward_debt.checked_add(rewards).unwrap();

            // Emit claim event
            emit!(ClaimEvent {
                owner: ctx.accounts.owner.key(),
                position_id: staker.position_id,
                rewards,
                timestamp: clock.unix_timestamp,
            });
        }

        Ok(())
    }

    pub fn update_pool(ctx: Context<UpdatePool>) -> Result<()> {
        require!(
            ctx.accounts.authority.key() == ctx.accounts.pool.authority,
            StakingError::Unauthorized
        );
        
        let pool = &mut ctx.accounts.pool;
        pool.paused = !pool.paused;

        emit!(EmergencyEvent {
            paused: pool.paused,
            timestamp: Clock::get()?.unix_timestamp,
        });

        Ok(())
    }
}

#[derive(Accounts)]
#[instruction(reward_pool_amount: u64)]
pub struct InitializePool<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = authority,
        space = StakingPool::LEN + 8,
        seeds = [b"pool_v3"],
        bump
    )]
    pub pool: Account<'info, StakingPool>,

    pub zara_mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
#[instruction(amount: u64)]
pub struct Stake<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        token::mint = zara_mint,
        token::authority = owner,
    )]
    pub staker_token_account: Account<'info, TokenAccount>,

    #[account(
        init,
        payer = owner,
        space = StakerInfo::LEN + 8,
        seeds = [
            b"staker",
            owner.key().as_ref(),
            &pool.next_position_id.to_le_bytes()
        ],
        bump
    )]
    pub staker: Account<'info, StakerInfo>,

    #[account(
        mut,
        seeds = [b"pool_v3"],
        bump,
        has_one = zara_mint @ StakingError::InvalidMint,
    )]
    pub pool: Account<'info, StakingPool>,

    #[account(
        mut,
        token::mint = zara_mint,
        token::authority = pool,
    )]
    pub stake_vault: Account<'info, TokenAccount>,

    #[account(
        mut,
        token::mint = zara_mint,
        token::authority = pool,
    )]
    pub reward_vault: Account<'info, TokenAccount>,

    /// CHECK: This is safe because we validate it in the constraint
    pub authority: UncheckedAccount<'info>,

    pub zara_mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

impl<'info> Stake<'info> {
    pub fn process(&mut self, amount: u64) -> Result<()> {
        require!(!self.pool.paused, StakingError::PoolPaused);
        require!(
            self.authority.key() == self.pool.authority,
            StakingError::Unauthorized
        );

        let clock = Clock::get()?;

        // Enhanced tier validation with proper logging and max amounts
        let (tier, lock_period, apr) = if amount >= ELITE_TIER_AMOUNT {
            msg!("Elite tier selected - {}/{} ZARA staked", amount, ELITE_TIER_AMOUNT);
            (StakingTier::Elite, ELITE_LOCK, ELITE_APR)
        } else if amount >= PREMIUM_TIER_AMOUNT {
            msg!("Premium tier selected - {}/{} ZARA staked", amount, PREMIUM_TIER_AMOUNT);
            (StakingTier::Premium, PREMIUM_LOCK, PREMIUM_APR)
        } else if amount >= BASIC_TIER_AMOUNT {
            msg!("Basic tier selected - {}/{} ZARA staked", amount, BASIC_TIER_AMOUNT);
            (StakingTier::Basic, BASIC_LOCK, BASIC_APR)
        } else {
            msg!("Insufficient stake amount: {}. Minimum required: {}", amount, BASIC_TIER_AMOUNT);
            return Err(StakingError::InsufficientStakeAmount.into());
        };

        // Validate stake vault has enough capacity (prevent overflow)
        require!(
            self.stake_vault.amount.checked_add(amount).is_some(),
            StakingError::MaxStakeExceeded
        );

        // Transfer tokens to stake vault
        token::transfer(
            CpiContext::new(
                self.token_program.to_account_info(),
                Transfer {
                    from: self.staker_token_account.to_account_info(),
                    to: self.stake_vault.to_account_info(),
                    authority: self.owner.to_account_info(),
                },
            ),
            amount,
        )?;

        // Get next position ID with overflow check
        let position_id = self.pool.next_position_id;

        // Set staker info
        self.staker.owner = self.owner.key();
        self.staker.pool = self.pool.key();
        self.staker.position_id = position_id;
        self.staker.stake_start_time = clock.unix_timestamp;
        self.staker.stake_end_time = clock.unix_timestamp + lock_period;
        self.staker.last_claim_time = clock.unix_timestamp;
        self.staker.staked_amount = amount;
        self.staker.tier = tier;
        self.staker.apr = apr;
        self.staker.reward_debt = 0;

        // Update pool info with checked math
        self.pool.total_staked = self.pool.total_staked.checked_add(amount).unwrap();
        self.pool.staker_count = self.pool.staker_count.checked_add(1).unwrap();
        self.pool.next_position_id = self.pool.next_position_id.checked_add(1).unwrap();

        // Emit detailed stake event
        emit!(StakeEvent {
            owner: self.owner.key(),
            position_id,
            amount,
            tier,
            lock_period,
            apr,
            timestamp: clock.unix_timestamp,
        });

        Ok(())
    }
}

#[derive(Accounts)]
pub struct StakeAccounts<'info> {
    pub owner: Signer<'info>,
    pub staker_token_account: Box<Account<'info, TokenAccount>>,
    pub staker: Box<Account<'info, StakerInfo>>,
    pub pool: Box<Account<'info, StakingPool>>,
}

#[derive(Accounts)]
pub struct StakeVaults<'info> {
    pub stake_vault: Box<Account<'info, TokenAccount>>,
    pub reward_vault: Box<Account<'info, TokenAccount>>,
    pub zara_mint: Box<Account<'info, Mint>>,
}

#[derive(Accounts)]
pub struct StakePrograms<'info> {
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Unstake<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        token::mint = zara_mint,
        token::authority = owner,
    )]
    pub staker_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        close = owner,
        seeds = [
            b"staker",
            owner.key().as_ref(),
            &staker.position_id.to_le_bytes()
        ],
        bump,
        has_one = owner,
        has_one = pool,
    )]
    pub staker: Account<'info, StakerInfo>,

    #[account(
        mut,
        seeds = [b"pool_v3"],
        bump,
    )]
    pub pool: Account<'info, StakingPool>,

    #[account(
        mut,
        token::mint = zara_mint,
        token::authority = pool,
    )]
    pub stake_vault: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        token::mint = zara_mint,
        token::authority = pool,
    )]
    pub reward_vault: Box<Account<'info, TokenAccount>>,

    pub zara_mint: Box<Account<'info, Mint>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ClaimRewards<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(
        mut,
        token::mint = zara_mint,
        token::authority = owner,
    )]
    pub staker_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        seeds = [
            b"staker",
            owner.key().as_ref(),
            &staker.position_id.to_le_bytes()
        ],
        bump,
        has_one = owner,
        has_one = pool,
    )]
    pub staker: Account<'info, StakerInfo>,

    #[account(
        mut,
        seeds = [b"pool_v3"],
        bump,
    )]
    pub pool: Account<'info, StakingPool>,

    #[account(
        mut,
        token::mint = zara_mint,
        token::authority = pool,
    )]
    pub reward_vault: Box<Account<'info, TokenAccount>>,

    pub zara_mint: Box<Account<'info, Mint>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdatePool<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(mut)]
    pub pool: Account<'info, StakingPool>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum StakingTier {
    #[default]
    Basic,
    Premium,
    Elite,
}

#[account]
#[derive(Default)]
pub struct StakingPool {
    pub authority: Pubkey,
    pub zara_mint: Pubkey,
    pub total_staked: u64,
    pub staker_count: u64,
    pub paused: bool,
    pub reward_pool_amount: u64,
    pub next_position_id: u64,
}

#[account]
#[derive(Default)]
pub struct StakerInfo {
    pub owner: Pubkey,
    pub pool: Pubkey,
    pub position_id: u64,
    pub stake_start_time: i64,
    pub stake_end_time: i64,
    pub last_claim_time: i64,
    pub staked_amount: u64,
    pub tier: StakingTier,
    pub apr: u64,
    pub reward_debt: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default)]
pub struct PoolBumps {
    pub pool: u8,
    pub stake_vault: u8,
    pub reward_vault: u8,
}

#[error_code]
pub enum StakingError {
    #[msg("Minimum staking duration not met")]
    MinStakeDurationNotMet,
    #[msg("Insufficient stake amount (min 5,000 ZARA)")]
    InsufficientStakeAmount,
    #[msg("Unauthorized access")]
    Unauthorized,
    #[msg("Pool is paused")]
    PoolPaused,
    #[msg("Pool is not paused")]
    PoolNotPaused,
    #[msg("Excessive reward pool amount")]
    ExcessiveRewardPool,
    #[msg("Annual reward limit exceeded")]
    AnnualRewardLimitExceeded,
    #[msg("Invalid mint address")]
    InvalidMint,
    #[msg("Invalid amount for selected tier")]
    InvalidTierAmount,
    #[msg("Insufficient reward pool balance")]
    InsufficientRewardPool,
    #[msg("Insufficient stake vault balance")]
    InsufficientStakeVault,
    #[msg("Maximum stake amount exceeded for tier")]
    MaxStakeExceeded,
}

#[event]
pub struct PoolInitializedEvent {
    pub authority: Pubkey,
    pub reward_pool: u64,
    pub timestamp: i64,
}

#[event]
pub struct StakeEvent {
    pub owner: Pubkey,
    pub position_id: u64,
    pub amount: u64,
    pub tier: StakingTier,
    pub lock_period: i64,
    pub apr: u64,
    pub timestamp: i64,
}

#[event]
pub struct RewardRateUpdateEvent {
    pub old_rate: u64,
    pub new_rate: u64,
    pub timestamp: i64,
}

#[event]
pub struct EmergencyEvent {
    pub paused: bool,
    pub timestamp: i64,
}

#[event]
pub struct UnstakeEvent {
    pub owner: Pubkey,
    pub position_id: u64,
    pub amount: u64,
    pub rewards: u64,
    pub timestamp: i64,
}

#[event]
pub struct EmergencyUnstakeEvent {
    pub owner: Pubkey,
    pub position_id: u64,
    pub amount: u64,
    pub timestamp: i64,
}

#[event]
pub struct ClaimEvent {
    pub owner: Pubkey,
    pub position_id: u64,
    pub rewards: u64,
    pub timestamp: i64,
}

impl StakingPool {
    pub const LEN: usize = 32 + // authority
        32 + // zara_mint
        8 + // total_staked
        8 + // staker_count
        8 + // reward_pool_amount
        1 + // paused
        8; // next_position_id
}

impl StakerInfo {
    pub const LEN: usize = 32 + // owner
        32 + // pool
        8 + // position_id
        8 + // stake_start_time
        8 + // stake_end_time
        8 + // last_claim_time
        8 + // staked_amount
        1 + // tier
        8 + // apr
        8; // reward_debt
}

// Enhanced reward calculation
fn calculate_rewards(
    staked_amount: u64,
    last_claim_time: i64,
    current_time: i64,
    apr: u64,
) -> Result<u64> {
    let time_staked = current_time.checked_sub(last_claim_time).unwrap();
    let rewards = (staked_amount as u128)
        .checked_mul(apr as u128).unwrap()
        .checked_mul(time_staked as u128).unwrap()
        .checked_div(365 * 24 * 60 * 60 * 10000).unwrap(); // APR calculation
    Ok(rewards as u64)
}

// Helper function to calculate max annual rewards
fn calculate_annual_rewards(staked_amount: u64, apr: u64) -> Result<u64> {
    Ok((staked_amount as u128)
        .checked_mul(apr as u128).unwrap()
        .checked_div(10000).unwrap() as u64)
} 