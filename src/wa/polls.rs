// Creating polls, decrypting and casting votes.
use super::*;

pub(super) async fn run(ctx: &Ctx, cmd: Cmd) {
    let epoch = ctx.epoch;
    match cmd {
        Cmd::CreatePoll {
            jid,
            question,
            options,
            selectable,
        } => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                let Some(chat) = parse_jid(&jid) else {
                    action_failed(epoch, "Invalid destination JID");
                    return;
                };
                match client
                    .polls()
                    .create(chat, &question, &options, selectable)
                    .await
                {
                    Ok((sent, secret)) => {
                        let id = sent.message_id.clone();
                        let message = (*sent.message).clone();
                        crate::bridge::wa_apply_at(epoch, move |b| {
                            b.on_poll_created(&jid, &id, message, &secret)
                        });
                    }
                    Err(e) => action_failed(epoch, format!("[wa] poll failed: {e}")),
                }
            });
        }
        Cmd::DecryptVote {
            jid,
            poll_id,
            creator,
            voter,
            secret,
            payload,
            iv,
            timestamp_ms,
        } => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                let (Some(creator_jid), Some(voter_jid)) = (parse_jid(&creator), parse_jid(&voter))
                else {
                    return;
                };
                let ciphertext = whatsapp_rust::features::PollVoteCiphertext {
                    enc_payload: &payload,
                    enc_iv: &iv,
                };
                match client
                    .polls()
                    .decrypt_vote(ciphertext, &secret, &poll_id, &creator_jid, &voter_jid)
                    .await
                {
                    Ok(hashes) => {
                        let picked: Vec<String> = hashes
                            .iter()
                            .map(|hash| hash.iter().map(|byte| format!("{byte:02x}")).collect())
                            .collect();
                        crate::bridge::wa_apply_at(epoch, move |b| {
                            b.on_poll_vote(&jid, &poll_id, &voter, picked, timestamp_ms)
                        });
                    }
                    Err(e) => eprintln!("[wa] poll vote could not be opened: {e}"),
                }
            });
        }
        Cmd::PollVote {
            jid,
            id,
            creator,
            secret,
            options,
        } => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                let (Some(chat), Some(creator_jid)) = (parse_jid(&jid), parse_jid(&creator)) else {
                    action_failed(epoch, "Invalid destination JID");
                    return;
                };
                match client
                    .polls()
                    .vote(chat, &id, &creator_jid, &secret, &options)
                    .await
                {
                    Ok(_) => crate::bridge::wa_apply_at(epoch, move |b| {
                        b.on_poll_vote_sent(&jid, &id, &options)
                    }),
                    Err(e) => action_failed(epoch, format!("[wa] poll vote failed: {e}")),
                }
            });
        }
        _ => unreachable!("not a polls command"),
    }
}
