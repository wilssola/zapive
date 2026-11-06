// Group metadata and membership.
use super::*;

pub(super) async fn run(ctx: &Ctx, cmd: Cmd) {
    match cmd {
        Cmd::FetchGroupMeta(jid) => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                let Some(target) = parse_jid(&jid) else {
                    return;
                };
                match client.groups().get_metadata(&target).await {
                    Ok(meta) => {
                        let members = meta
                            .participants
                            .iter()
                            .map(|p| crate::store::Member {
                                jid: p.jid.to_non_ad_string(),
                                phone: p
                                    .phone_number
                                    .as_ref()
                                    .map(|j| j.to_non_ad_string())
                                    .unwrap_or_default(),
                                name: p
                                    .details
                                    .as_ref()
                                    .and_then(|d| d.display_name.as_ref())
                                    .map(|n| n.to_string())
                                    .unwrap_or_default(),
                                admin: p.is_admin(),
                            })
                            .collect();
                        let snap = GroupSnapshot {
                            jid,
                            subject: meta.subject.clone(),
                            desc: meta.description.clone().unwrap_or_default(),
                            announce: meta.is_announcement,
                            ephemeral: meta
                                .ephemeral
                                .as_ref()
                                .and_then(|e| e.expiration)
                                .unwrap_or(0),
                            members,
                        };
                        ui_apply(move |b| b.on_group_meta(snap));
                    }
                    Err(e) => eprintln!("[wa] group metadata failed for {jid}: {e}"),
                }
            });
        }
        Cmd::LeaveGroup(jid) => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                let Some(target) = parse_jid(&jid) else {
                    return;
                };
                match client.groups().leave(target).await {
                    Ok(()) => ui_apply(move |b| b.on_group_left(&jid)),
                    Err(e) => eprintln!("[wa] leave group failed: {e}"),
                }
            });
        }
        Cmd::AddMembers { jid, members } => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                let Some(target) = parse_jid(&jid) else {
                    return;
                };
                let who: Vec<whatsapp_rust::Jid> =
                    members.iter().filter_map(|m| parse_jid(m)).collect();
                match client.groups().add_participants(target, &who).await {
                    Ok(results) => {
                        let failed: Vec<String> = results
                            .iter()
                            .filter(|r| r.error.is_some())
                            .map(|r| r.jid.to_non_ad_string())
                            .collect();
                        ui_apply(move |b| b.on_members_added(&jid, &failed));
                    }
                    Err(e) => eprintln!("[wa] add members failed: {e}"),
                }
            });
        }
        Cmd::FetchCommonGroups { jid, identities } => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                let result = client.groups().get_participating().await;
                let groups = match result {
                    Ok(groups) => Some(
                        groups
                            .into_iter()
                            .filter_map(|(group_jid, meta)| {
                                let shared = meta.participants.iter().any(|participant| {
                                    std::iter::once(&participant.jid)
                                        .chain(participant.phone_number.iter())
                                        .chain(participant.lid.iter())
                                        .any(|identity| {
                                            identities.contains(&identity.to_non_ad_string())
                                        })
                                });
                                shared.then(|| (group_jid.to_non_ad_string(), meta.subject))
                            })
                            .collect::<Vec<_>>(),
                    ),
                    Err(error) => {
                        log::warn!("fetch common groups failed: {error}");
                        None
                    }
                };
                ui_apply(move |b| b.on_common_groups(&jid, groups));
            });
        }
        Cmd::FetchGroups => {
            let client = ctx.client.clone();
            tokio::spawn(async move {
                match client.groups().get_participating().await {
                    Ok(groups) => {
                        let list: Vec<(String, String, bool, Option<String>)> = groups
                            .iter()
                            .map(|(jid, meta)| {
                                (
                                    jid.to_non_ad_string(),
                                    meta.subject.clone(),
                                    meta.is_parent_group,
                                    meta.parent_group_jid.as_ref().map(|j| j.to_non_ad_string()),
                                )
                            })
                            .collect();
                        ui_apply(move |b| b.on_groups(&list));
                    }
                    Err(e) => eprintln!("[wa] fetch groups failed: {e}"),
                }
            });
        }
        _ => unreachable!("not a groups command"),
    }
}
