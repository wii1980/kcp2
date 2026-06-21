use crate::consts::*;

#[inline]
pub(crate) fn time_diff(later: u32, earlier: u32) -> i32 {
    later.wrapping_sub(earlier) as i32
}

pub(crate) fn check_resend(
    xmit: u32,
    resendts: u32,
    fastack: u32,
    current: u32,
    fastresend: u32,
    fastlimit: u32,
) -> ResendDecision {
    if xmit == 0 {
        ResendDecision::FirstSend
    } else if time_diff(current, resendts) >= 0 {
        ResendDecision::Timeout
    } else if fastack >= fastresend && (xmit <= fastlimit || fastlimit == 0) {
        ResendDecision::FastRetransmit
    } else {
        ResendDecision::NoResend
    }
}

pub(crate) enum ResendDecision {
    FirstSend,
    Timeout,
    FastRetransmit,
    NoResend,
}

pub(crate) fn calculate_rto(nodelay: bool, rx_rto: u32, rx_minrto: u32) -> u32 {
    if nodelay {
        rx_rto.min(rx_minrto.saturating_mul(2))
    } else {
        rx_rto
    }
}

pub(crate) fn update_rto_for_retransmit(nodelay: bool, rto: u32, rx_rto: u32) -> u32 {
    if nodelay {
        rto + rto / 2
    } else {
        rto + rto.max(rx_rto)
    }
}

pub(crate) fn update_congestion(
    snd_una: u32,
    old_una: u32,
    cwnd: u16,
    ssthresh: u16,
    rmt_wnd: u16,
    mss: usize,
    incr: u32,
) -> (u16, u16, u32) {
    if time_diff(snd_una, old_una) > 0 && cwnd < rmt_wnd {
        let mss32 = mss as u32;
        if cwnd < ssthresh {
            let cwnd = cwnd + 1;
            let incr = incr + mss32;
            (cwnd, ssthresh, incr)
        } else {
            let incr = if incr < mss32 { mss32 } else { incr };
            let incr = incr + ((mss32 * mss32) / incr + mss32 / 16);
            let cwnd = if (cwnd + 1) as u64 * mss as u64 <= incr as u64 {
                incr.div_ceil(mss32) as u16
            } else {
                cwnd + 1
            };
            let cwnd = cwnd.min(rmt_wnd);
            let incr = if cwnd == rmt_wnd {
                rmt_wnd as u32 * mss32
            } else {
                incr
            };
            (cwnd, ssthresh, incr)
        }
    } else {
        (cwnd, ssthresh, incr)
    }
}

pub(crate) fn congestion_fast_retransmit(
    snd_nxt: u32,
    snd_una: u32,
    resent: u32,
    mss: usize,
) -> (u16, u16, u32) {
    let inflight = snd_nxt.wrapping_sub(snd_una);
    let ssthresh = (inflight / 2).max(THRESH_MIN as u32) as u16;
    let resent_clamped = resent.min(u16::MAX as u32) as u16;
    let cwnd = ssthresh + resent_clamped;
    let incr = cwnd as u32 * mss as u32;
    (cwnd, ssthresh, incr)
}

pub(crate) fn congestion_loss(cwnd: u16, mss: usize) -> (u16, u16, u32) {
    let ssthresh = (cwnd / 2).max(THRESH_MIN);
    (1, ssthresh, mss as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_congestion_fast_retransmit_normal() {
        let (cwnd, ssthresh, incr) = congestion_fast_retransmit(100, 0, 3, 1400);
        assert_eq!(ssthresh, 50);
        assert_eq!(cwnd, 53);
        assert_eq!(incr, 53 * 1400);
    }

    #[test]
    fn test_congestion_fast_retransmit_zero_inflight() {
        let (cwnd, ssthresh, incr) = congestion_fast_retransmit(0, 0, 5, 1400);
        assert_eq!(ssthresh, 2);
        assert_eq!(cwnd, 7);
        assert_eq!(incr, 7 * 1400);
    }

    #[test]
    fn test_congestion_fast_retransmit_large_resent() {
        let (cwnd, ssthresh, incr) = congestion_fast_retransmit(100, 0, 1000, 1400);
        assert_eq!(ssthresh, 50);
        assert_eq!(cwnd, 1050);
        assert_eq!(incr, 1050 * 1400);
    }

    #[test]
    fn test_congestion_fast_retransmit_clamp_expression() {
        let a_large: u32 = u32::MAX;
        let clamped = a_large.min(u16::MAX as u32) as u16;
        assert_eq!(clamped, u16::MAX);

        let small: u32 = 42;
        let unchanged = small.min(u16::MAX as u32) as u16;
        assert_eq!(unchanged, 42);
    }
}
