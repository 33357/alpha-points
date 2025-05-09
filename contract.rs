//! Timed Sell Order — Solana Anchor program
//! Allows a seller to delegate (approve) SPL tokens to a PDA so that anyone can
//! purchase them before a user‑defined deadline.  After the deadline the seller
//! can cancel and the delegate is revoked.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Approve, Revoke, Token, TokenAccount, Transfer};

// -----------------------------------------------------------------------------
// Declare program id (update with `solana address -k target/idl/…` after deploy)
// -----------------------------------------------------------------------------
declare_id!("S3LLorD3r2hV1W6C4METH1NVQGdcvJxdKmxhZz7D3Lg");

// ============================================================================
// Program entrypoints
// ============================================================================
#[program]
pub mod timed_sell_order {
    use super::*;

    /// Create a new sell order and delegate `amount` tokens from the seller’s
    /// token account to the program‑derived *order authority*.
    pub fn create_sell_order(
        ctx: Context<CreateSellOrder>,
        amount: u64,
        price_per_token: u64, // denominated in **lamports** for simplicity
        deadline: i64,        // unix timestamp (UTC)
    ) -> Result<()> {
        // --- sanity checks ---------------------------------------------------
        require!(amount > 0, SellError::InvalidAmount);
        require!(price_per_token > 0, SellError::InvalidPrice);
        require!(deadline > Clock::get()?.unix_timestamp, SellError::DeadlineInPast);

        // --- persist order data ---------------------------------------------
        let order = &mut ctx.accounts.sell_order;
        order.seller = ctx.accounts.seller.key();
        order.token_mint = ctx.accounts.seller_token_account.mint;
        order.token_account = ctx.accounts.seller_token_account.key();
        order.amount_remaining = amount;
        order.price_per_token = price_per_token;
        order.deadline = deadline;
        order.bump = *ctx.bumps.get("order_authority").unwrap();

        // --- delegate SPL tokens to PDA -------------------------------------
        token::approve(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Approve {
                    to: ctx.accounts.seller_token_account.to_account_info(),
                    delegate: ctx.accounts.order_authority.to_account_info(),
                    authority: ctx.accounts.seller.to_account_info(),
                },
            ),
            amount,
        )?;

        Ok(())
    }

    /// Anyone can buy up to the remaining `amount` of tokens *before* the
    /// deadline by paying `amount * price_per_token` lamports to the seller.
    pub fn buy(ctx: Context<Buy>, amount: u64) -> Result<()> {
        let order = &mut ctx.accounts.sell_order;

        // --- checks ----------------------------------------------------------
        require!(Clock::get()?.unix_timestamp <= order.deadline, SellError::OrderExpired);
        require!(amount > 0 && amount <= order.amount_remaining, SellError::InvalidAmount);

        // --- handle payment --------------------------------------------------
        let total_price = amount
            .checked_mul(order.price_per_token)
            .ok_or(SellError::MathOverflow)?;

        **ctx.accounts.buyer.try_borrow_mut_lamports()? -= total_price;
        **ctx.accounts.seller.try_borrow_mut_lamports()? += total_price;

        // --- transfer tokens -------------------------------------------------
        let seeds: &[&[&[u8]]] = &[&[
            order.seller.as_ref(),
            order.token_account.as_ref(),
            &[order.bump],
        ]];

        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.seller_token_account.to_account_info(),
                    to: ctx.accounts.buyer_token_account.to_account_info(),
                    authority: ctx.accounts.order_authority.to_account_info(),
                },
                seeds,
            ),
            amount,
        )?;

        order.amount_remaining -= amount;
        Ok(())
    }

    /// Seller can cancel the order *any time* (even before deadline).  All
    /// remaining tokens stay in the seller’s account and delegate is revoked.
    pub fn cancel(ctx: Context<Cancel>) -> Result<()> {
        token::revoke(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Revoke {
                    source: ctx.accounts.seller_token_account.to_account_info(),
                    authority: ctx.accounts.seller.to_account_info(),
                },
            ),
        )?;
        Ok(())
    }
}

// ============================================================================
// Accounts structs
// ============================================================================
#[derive(Accounts)]
pub struct CreateSellOrder<'info> {
    /// Signer creating the order
    #[account(mut)]
    pub seller: Signer<'info>,

    /// Seller’s SPL token account holding the tokens for sale
    #[account(mut, owner = token_program.key())]
    pub seller_token_account: Account<'info, TokenAccount>,

    /// PDA that becomes the *delegate/authority* for token transfers
    #[account(
        seeds = [seller.key().as_ref(), seller_token_account.key().as_ref()],
        bump,
    )]
    pub order_authority: SystemAccount<'info>,

    /// Order state account (PDA)
    #[account(
        init,
        payer = seller,
        space = 8 + SellOrder::SIZE,
        seeds = [b"sell_order", seller.key().as_ref(), seller_token_account.key().as_ref()],
        bump,
    )]
    pub sell_order: Account<'info, SellOrder>,

    /// Programs & sysvars
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Buy<'info> {
    #[account(mut)]
    pub buyer: Signer<'info>,

    /// Seller receives payment
    #[account(mut)]
    pub seller: SystemAccount<'info>,

    /// Order state
    #[account(
        mut,
        has_one = seller,
        has_one = token_account,
        seeds = [b"sell_order", seller.key().as_ref(), token_account.key().as_ref()],
        bump = sell_order.bump,
    )]
    pub sell_order: Account<'info, SellOrder>,

    /// Same token account as recorded in the order
    #[account(mut)]
    pub token_account: Account<'info, TokenAccount>,

    /// Buyer’s token account to receive tokens
    #[account(mut)]
    pub buyer_token_account: Account<'info, TokenAccount>,

    /// PDA delegate that actually moves tokens
    #[account(
        seeds = [seller.key().as_ref(), token_account.key().as_ref()],
        bump = sell_order.bump,
    )]
    pub order_authority: SystemAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Cancel<'info> {
    #[account(mut)]
    pub seller: Signer<'info>,

    #[account(
        mut,
        close = seller,
        has_one = seller,
        has_one = token_account,
        seeds = [b"sell_order", seller.key().as_ref(), token_account.key().as_ref()],
        bump = sell_order.bump,
    )]
    pub sell_order: Account<'info, SellOrder>,

    #[account(mut)]
    pub token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

// ============================================================================
// State
// ============================================================================
#[account]
pub struct SellOrder {
    pub seller: Pubkey,
    pub token_mint: Pubkey,
    pub token_account: Pubkey,
    pub amount_remaining: u64,
    pub price_per_token: u64,
    pub deadline: i64,
    pub bump: u8,
}

impl SellOrder {
    // account discriminator (8) + 32*3 + 8*3 + 1 = 8 + 96 + 24 + 1 = 129
    pub const SIZE: usize = 129;
}

// ============================================================================
// Errors
// ============================================================================
#[error_code]
pub enum SellError {
    #[msg("Amount must be positive")]
    InvalidAmount,
    #[msg("Price must be positive")]
    InvalidPrice,
    #[msg("Deadline must be in the future")]
    DeadlineInPast,
    #[msg("The sell order has already expired")] 
    OrderExpired,
    #[msg("Math overflow")] 
    MathOverflow,
}