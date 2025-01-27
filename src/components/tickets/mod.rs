//! Ticket manager

mod archive;

use std::path::PathBuf;
use crate::{log_error, log_warn};
use futures_locks::RwLock;
use cddio_core::{message, ApplicationCommandEmbed};
use cddio_macros::component;
use serde::{Serialize, Deserialize};
use serenity::{
    client::Context,
    model::{id::*, channel::Message, event::ReadyEvent},
    model::application::interaction:: {
        message_component::MessageComponentInteraction
    }, builder::CreateSelectMenuOption
};

use super::utils::data::Data;

/// Le composant de gestion des tickets
pub struct Tickets {
    /// Données persistantes
    data: RwLock<Data<DataTickets>>,
    /// Dossier de sauvegarde des tickets
    /// 
    /// Dès que les tickets sont supprimés, ils sont enregistrés dans ce dossier.
    archives_folder: PathBuf
}

/// Données persistantes du composant
/// 
/// A chaque écriture dans le fichier de données, le fichier est sauvegardé
#[derive(Serialize, Deserialize, Default, Debug)]
struct DataTickets {
    /// Identifiants du channel et du message pour choisir le type de ticket
    /// Ces identifiants est enregistré pour pouvoir le remplacer si nécessaire
    msg_choose: Option<(u64, u64)>,
    /// [Catégories] de tickets
    /// 
    /// [Catégories]: CategoryTicket
    categories: Vec<CategoryTicket>,
}

/// Catégorie de tickets
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
struct CategoryTicket {
    /// Nom de la catégorie
    name: String, 
    /// Préfix de ticket
    /// 
    /// Le préfix est utilisé pour créer le titre d'un ticket tel que 
    /// `<prefix>_<username>`
    prefix: String,
    /// Identifiant de la catégorie Discord
    id: u64,
    /// Description de la catégorie
    desc: Option<String>,
    /// Tickets créés dans cette catégorie
    tickets: Vec<String>,
    #[serde(default)]
    hidden: bool,
}

impl From<CategoryTicket> for CreateSelectMenuOption {
    fn from(ticket: CategoryTicket) -> Self {
        let mut menu_option = CreateSelectMenuOption::new(&ticket.name, &ticket.name);
        menu_option
            .description(ticket.desc.unwrap_or_default());
        menu_option
    }
} 
impl From<&CategoryTicket> for CreateSelectMenuOption {
    fn from(ticket: &CategoryTicket) -> Self {
        let mut menu_option = CreateSelectMenuOption::new(&ticket.name, &ticket.name);
        menu_option
            .description(ticket.desc.clone().unwrap_or_default());
        menu_option
    }
}
impl CategoryTicket {
    fn to_message(&self, title: &str) -> message::Message {
        let mut msg = message::Message::new();
        let mut embed = message::Embed::default();
        embed.color(message::COLOR_INFO);
        embed.title(title);
        embed.field(&self.name, self.desc.as_ref().map(|v| v.as_str()).unwrap_or("*Aucune description*"), false);
        msg.add_embed(|e| {*e=embed; e});
        msg
    }
}

impl Tickets {
    /// Créer un nouveau composant de gestion des tickets
    pub fn new() -> Self {
        Self {
            data: RwLock::new(Data::from_file("tickets").unwrap()),
            archives_folder: PathBuf::from("data/archives/tickets")
        }
    }
}

#[component]
#[group(name="tickets", description="Gestion des tickets")]
#[group(parent="tickets", name="categories", description="Gestion des catégories de tickets")]
#[group(name="ticket", description="Commandes dans un ticket")]
impl Tickets {
    #[event(Ready)]
    async fn on_ready(&self, ctx: &Context, _:&ReadyEvent) {
        let msg_choose = {
            let data = self.data.read().await;
            let data = data.read();
            data.msg_choose.clone()
        };
        if let Some((chan_id, msg_id)) = msg_choose {
            let mut msg = match ChannelId(chan_id).message(ctx, msg_id).await {
                Ok(msg) => msg,
                Err(err) => {
                    log_warn!("Erreur lors de la récupération du message du menu: {:?}", err);
                    self.reset_message_choose(None).await;
                    return;
                }
            };
            if let Err(err) = self.update_menu(ctx, &mut msg).await {
                log_warn!("Erreur lors de la mise à jour du menu: {}", err);
                self.reset_message_choose(None).await;
            }
        }
    }
    #[command(group="tickets", description="Assigne le salon de création de tickets")]
    async fn set_channel(&self, ctx: &Context, app_cmd: ApplicationCommandEmbed<'_>,
        #[argument(name="salon", description="Salon textuel")]
        chan: Option<ChannelId>
    ) {
        let resp = match app_cmd.delayed_response(ctx, true).await {
            Ok(resp) => resp,
            Err(err) => {
                log_error!("Erreur lors de la création de la réponse: {}", err);
                return;
            }
        };
        loop {
            let data = self.data.read().await;
            let data = data.read();
            if let Some((chan_id, msg_id)) = data.msg_choose {
                let msg = match ChannelId(chan_id).message(ctx, msg_id).await {
                    Ok(msg) => msg,
                    Err(err) => {
                        log_warn!("Erreur lors de la récupération du menu: {}", err);
                        break;
                    }
                };
                if let Err(err) = msg.delete(ctx).await {
                    log_warn!("Erreur lors de la récupération du message: {}", err);
                    break;
                }
            }
            break;
        }
        let channel = chan.unwrap_or(app_cmd.0.channel_id);

        let mut msg = match channel.send_message(ctx, |msg| msg.content("Sélectionnez le type de ticket que vous souhaitez créer :")).await {
            Ok(msg) => msg,
            Err(err) => {
                log_error!("Erreur lors de l'envoi du message: {:?}", err);
                return;
            }
        };
        self.update_menu(ctx, &mut msg).await.unwrap_or_else(|e| {
            log_error!("Erreur lors de la mise a jour du menu: {:?}", e);
        });
        {
            let mut data = self.data.write().await;
            let mut data = data.write();

            data.msg_choose = Some((channel.0, msg.id.0));
        }
        if let Err(err) = resp.send_message(message::success("Salon de création de tickets configuré")).await {
            log_error!("Erreur lors de l'envoi de la réponse: {:?}", err);
        }
    }
    #[command(group="ticket", name="close", description="Ferme le ticket actuel")]
    async fn ticket_close(&self, ctx: &Context, app_cmd: ApplicationCommandEmbed<'_>) {
        if let Err(e) = self.ticket_close_channel(ctx, app_cmd.0.channel_id).await {
            Self::send_error(ctx, app_cmd, e).await;
        }
    }
    #[command(group="categories", name="add", description="Ajoute une catégorie de ticket. À ne pas confondre avec les catégories discord")]
    async fn add_categorie(&self, ctx: &Context, app_cmd: ApplicationCommandEmbed<'_>,
        #[argument(name="nom", description="Nom de la catégorie")]
        name: String,
        #[argument(description="Catégorie Discord où les tickets seront créés", name="categorie_discord")]
        category_id: ChannelId,
        #[argument(description="Préfixe des tickets", name="prefix")]
        prefix: String,
        #[argument(description="Cacher la catégorie du menu de ticket ?")]
        hidden: bool,
        #[argument(description="Description de la catégorie", name="description")]
        desc: Option<String>
    ) {
        {
            let data = self.data.read().await;
            let data = data.read();
            for category in &data.categories {
                if category.name == name {
                    Self::send_error(ctx, app_cmd, "Cette catégorie existe déjà").await;
                    return;
                }
            }
        }
        {
            let mut data = self.data.write().await;
            let mut data = data.write();
            data.categories.push(CategoryTicket {
                name,
                prefix,
                id: category_id.0,
                desc,
                tickets: vec![],
                hidden
            });
        }
        {
            let data = self.data.read().await;
            let data = data.read();
            let msg = data.categories.last().unwrap().to_message("Catégorie créée");
            app_cmd.direct_response(ctx, msg).await.unwrap_or_else(|e| {
                log_error!("Erreur lors de l'envoi du message: {}", e);
            });
        }
    }
    #[command(group="categories", name="remove", description="Supprime une catégorie de ticket")]
    async fn remove_categorie(&self, ctx: &Context, app_cmd: ApplicationCommandEmbed<'_>,
        #[argument(name="nom", description="Nom de la catégorie")]
        name: String
    ) {
        let mut data = self.data.write().await;
        let mut data = data.write();
        let pos = match data.categories.iter().position(|category| category.name == name) {
            Some(pos) => pos,
            None => {
                app_cmd.direct_response(ctx, message::error("Cette catégorie n'existe pas")).await.unwrap_or_else(|e| {
                    log_error!("Erreur lors de l'envoi du message: {}", e);
                });
                return;
            }
        };
        let msg = data.categories[pos].to_message("Catégorie supprimée");
        data.categories.remove(pos);

        app_cmd.direct_response(ctx, msg).await.unwrap_or_else(|e| {
            log_error!("Erreur lors de l'envoi du message: {}", e);
        });
    }
    #[command(group="categories", name="list", description="Liste les catégories de ticket")]
    async fn list_categories(&self, ctx: &Context, app_cmd: ApplicationCommandEmbed<'_>) {
        let data = self.data.read().await;
        let data = data.read();
        let mut msg = message::Message::new();
        let mut embed = message::Embed::default();
        embed.title("Liste des catégories");
        embed.color(message::COLOR_INFO);
        for category in &data.categories {
            embed.field(&category.name, category.desc.clone().unwrap_or_else(|| "*Aucune desscription*".into()), false);
        }
        msg.add_embed(|e| {*e=embed; e});
        app_cmd.direct_response(ctx, msg).await.unwrap_or_else(|e| {
            log_error!("Erreur lors de l'envoi du message: {}", e);
        });
    }
    #[command(group="ticket", description="Ajoute une personne au ticket")]
    async fn add_member(&self, ctx: &Context, app_cmd: ApplicationCommandEmbed<'_>,
        #[argument(name="qui", description="Personne à ajouter au ticket")]
        personne: UserId
    ) {
        use serenity::model::{
            channel::{PermissionOverwrite, PermissionOverwriteType},
            permissions::Permissions,
        };
        let channel_id = app_cmd.0.channel_id;
        let delay_resp = match app_cmd.delayed_response(ctx, false).await {
            Ok(resp) => resp,
            Err(e) => {
                log_error!("Erreur lors de l'envoi du message: {}", e);
                return;
            }
        };
        let msg = loop {
            let guild_id = match app_cmd.0.guild_id {
                Some(guild_id) => guild_id,
                None => break message::error("Cette commande n'est pas disponible dans un DM"),
            };
            
            match self.is_a_ticket(ctx, channel_id).await  {
                Ok(true) => (),
                Ok(false) => break message::error("Ce salon n'est pas un ticket"),
                Err(e) => break message::error(e),
            }
            let is_staff = match Self::is_staff(ctx, guild_id, app_cmd.0.user.id).await {
                Ok(true) => true,
                Ok(false) => false,
                Err(e) => break message::error(e),
            };
            let is_owner = match Self::is_ticket_owner(ctx, channel_id, app_cmd.0.user.id).await {
                Ok(true) => true,
                Ok(false) => false,
                Err(e) => break message::error(e),
            };
            if !is_staff && !is_owner {
                break message::error("Vous n'avez pas la permission d'ajouter des membres au ticket.");
            }
            
            let username = personne.to_user(ctx).await.map(|u| super::utils::user_fullname(&u)).unwrap_or_else(|_| personne.0.to_string());
            break match channel_id.create_permission(ctx, &PermissionOverwrite {
                allow: Permissions::VIEW_CHANNEL,
                deny: Default::default(),
                kind: PermissionOverwriteType::Member(personne),
            }).await {
                Ok(_) => message::success(format!("{} a bien été ajoutée.", username)),
                Err(e) => message::error(format!("Impossible d'ajouter {}: {}", personne, e.to_string()))
            };
        };
        delay_resp.send_message(msg).await.unwrap_or_else(|e| {
            log_error!("Erreur lors de l'envoi du message: {}", e);
        });
    }
    #[message_component(custom_id="menu_ticket_create")]
    async fn on_menu_ticket_create(&self, ctx: &Context, msg: &MessageComponentInteraction) {
        use serenity::model::application::interaction::InteractionResponseType;
        let ok = match msg.create_interaction_response(ctx, |resp| {
            resp.kind(InteractionResponseType::DeferredChannelMessageWithSource)
                .interaction_response_data(|data| {
                    data.ephemeral(true)
                })
        }).await {
            Ok(_) => true,
            Err(e) => {
                log_warn!("Erreur lors de la création de l'interaction: {}", e);
                false
            }
        };
        let guild_id = match msg.guild_id {
            Some(guild_id) => guild_id,
            None => {
                log_error!("Le menu n'est pas dans un serveur");
                return;
            }
        };
        let user_id = msg.user.id;
        let category = {
            let category_name = match msg.data.values.iter().next() {
                Some(value) => value.clone(),
                None => {
                    log_error!("Aucun item n'a été sélectionné");
                    return;
                }
            };
            let data = self.data.read().await;
            let data = data.read();
            match data.categories.iter().find(|category| category.name == category_name) {
                Some(category) => category.clone(),
                None => {
                    log_error!("La catégorie {} n'existe pas", category_name);
                    return;
                }
            }
        };
        let result = match self.ticket_create(ctx, guild_id, user_id, category).await {
            Ok(result) => message::success(format!("Ticket créé: <#{}>", result)),
            Err(e) => {
                log_error!("Erreur lors de la création du ticket: {}", e);
                message::error(e)
            }
        };
        if ok {
            match msg.edit_original_interaction_response(ctx, |resp| {
                *resp = result.into();
                resp
            }).await {
                Ok(_) => (),
                Err(e) => {
                    log_error!("Erreur lors de la modification de l'interaction: {}", e);
                }
            }
        }
        
    }
    #[message_component(custom_id="button_ticket_close")]
    async fn on_button_ticket_close(&self, ctx: &Context, msg: &MessageComponentInteraction) {
        if let Err(e) = self.ticket_close_channel(ctx, msg.channel_id).await {
            log_error!("{}", e);
            msg.create_interaction_response(ctx, |resp|{
                resp.interaction_response_data(|inter| inter.content(e))
            }).await.unwrap_or_else(|e| {
                log_error!("Erreur lors de l'envoi d'une réponse d'interaction: {}", e);
            });
        }
    }
}

impl Tickets {
    async fn update_menu(&self, ctx: &Context, msg: &mut Message) -> serenity::Result<()>{
        let options = self.data.read().await.read().categories.iter().filter(|cat| !cat.hidden).map(|cat| cat.into()).collect::<Vec<_>>();
        msg.edit(ctx, |msg|{
            msg.components(|comp| {
                comp.create_action_row(|action| {
                    action.create_select_menu(|menu| {
                        menu.options(|opts|{
                            opts.set_options(options)
                        }).custom_id("menu_ticket_create")
                    })
                })
            })
        }).await
    }
    async fn send_error<D: std::fmt::Display>(ctx: &Context, app_cmd: ApplicationCommandEmbed<'_>, error: D) {
        log_error!("{}", error);
        let mut msg = message::Message::new();
        let mut embed = message::Embed::default();
        embed.color(message::COLOR_ERROR);
        embed.title("Erreur");
        embed.description(error);
        msg.add_embed(|e| {*e=embed; e});
        app_cmd.direct_response(ctx, msg).await.unwrap_or_else(|e| {
            log_error!("Erreur lors de l'envoi du message: {}", e);
        });
    }
    async fn ticket_close_channel(&self, ctx: &Context, channel_id: ChannelId) -> Result<(), String> {
        match self.is_a_ticket(ctx, channel_id).await {
            Ok(true) => (),
            Ok(false) => return Err("Ce n'est pas un ticket".to_string()),
            Err(e) => return Err(e),
        }
        if let Err(err) = archive::archive_ticket(ctx, channel_id).await {
            return Err(format!("Erreur lors de l'archivage du ticket: {}", err));
        }
        if let Err(err) = channel_id.delete(ctx).await {
            return Err(format!("Erreur lors de la suppression du ticket: {}", err));
        }
        Ok(())
    }
    async fn is_a_ticket(&self, ctx: &Context, channel_id: ChannelId) -> Result<bool, String> {
        use serenity::model::channel::Channel;
        let current_channel = match channel_id.to_channel(ctx).await {
            Ok(Channel::Guild(chan)) => chan,
            Ok(_) => return Ok(false),
            Err(e) => return Err(format!("Une erreur s'est produite lors de la récupération du channel: {}", e)),
        };
        let parent_channel = match current_channel.parent_id {
            Some(id) => id,
            None => return Ok(false),
        };
        {
            let data = self.data.read().await;
            let data = data.read();
            if let None = data.categories.iter().find(|cat| cat.id == parent_channel.0) {
                return Ok(false);
            }
        }
        Ok(true)
    }
    async fn is_ticket_owner(ctx: &Context, channel: ChannelId, user_by: UserId) -> Result<bool, String> {
        let pins = match channel.pins(ctx).await {
            Ok(pins) => pins,
            Err(e) => return Err(format!("{}", e))
        };
        let first_message = match pins.last() {
            Some(pin) => pin,
            None => return Ok(false)
        };
        Ok(first_message.mentions.iter().find(|m| m.id == user_by).is_some())
    }
    async fn is_staff(ctx: &Context, guild_id: GuildId, user_by: UserId) -> Result<bool, String> {
        let roles = match guild_id.roles(ctx).await {
            Ok(roles) => roles,
            Err(e) => return Err(format!("{}", e))
        };
        let staff_role = match roles.into_iter().find(|role| role.1.name == "staff") {
            Some(role) => role,
            None => return Err("Le rôle 'staff' n'existe pas.".to_string())
        };
        let member = match guild_id.member(ctx, user_by).await {
            Ok(member) => member,
            Err(e) => return Err(format!("{}", e))
        };
        Ok(member.roles.into_iter().find(|role| role == &staff_role.0).is_some())
    }
    async fn reset_message_choose(&self, new_ids: Option<(u64, u64)>) {
        self.data.write().await.write().msg_choose = new_ids;
    }
    async fn ticket_create(&self, ctx: &Context, guild_id: GuildId, user_id: UserId, category: CategoryTicket) -> Result<ChannelId, String> {
        use serenity::model::channel::{PermissionOverwrite, PermissionOverwriteType, ChannelType};
        use serenity::model::permissions::Permissions;
        use serenity::model::application::component::ButtonStyle;
        let role_staff = match guild_id.roles(ctx).await {
            Ok(roles) => {
                let role = roles.iter().find(|(_, role)| role.name == "staff");
                match role {
                    Some((role_id, _)) => *role_id,
                    None => {
                        log_error!("Une erreur s'est produite lors de la création du ticket: Le role 'staff' n'existe pas.");
                        return Err("Une erreur s'est produite lors de la création du ticket.".to_string());
                    }
                }
            },
            Err(e) => return Err(format!("Erreur lors de la récupération des roles: {}", e))
        };
        let everyone = RoleId(guild_id.0);
        
        let permissions = vec![
            PermissionOverwrite {
                allow: Permissions::VIEW_CHANNEL,
                deny: Permissions::default(),
                kind: PermissionOverwriteType::Member(user_id),
            },
            PermissionOverwrite {
                allow: Permissions::VIEW_CHANNEL,
                deny: Permissions::default(),
                kind: PermissionOverwriteType::Role(role_staff),
            },
            PermissionOverwrite {
                allow: Permissions::default(),
                deny: Permissions::VIEW_CHANNEL,
                kind: PermissionOverwriteType::Role(everyone),
            },
        ];
        let username = match user_id.to_user(ctx).await {
            Ok(user) => user.name,
            Err(_) => user_id.to_string()
        };
        let new_channel = match guild_id.create_channel(ctx, |chan| {
            chan
                .name(format!("{}-{}", category.prefix, username))
                .kind(ChannelType::Text)
                .category(category.id)
                .permissions(permissions)
        }).await {
            Ok(chan) => chan,
            Err(e) => return Err(format!("Erreur lors de la création du ticket: {}", e))
        };
        let mut msg_prez = match new_channel.say(ctx, format!("Hey <@{}>, par ici !\nDès que tu as fini avec le ticket, appuie sur le bouton \"Fermer le ticket\".", user_id.0)).await {
            Ok(msg) => msg,
            Err(e) => return Err(format!("Erreur pendent l'envoi du message de presentation: {}\nLe salon a tout de même été créé: <#{}>", e, new_channel.id.0))
        };
        msg_prez.edit(ctx, |msg| {
            msg.components(|cmps| {
                cmps.create_action_row(|action|{
                    action.create_button(|button|{
                        button
                            .label("Fermer le ticket")
                            .style(ButtonStyle::Danger)
                            .custom_id("button_ticket_close")
                    })
                })
            })
        }).await.unwrap_or_else(|e| {
            log_warn!("Erreur lors de la mise en place du bouton du message de présentation: {}", e);
        });
        msg_prez.pin(ctx).await.unwrap_or_else(|e| {
            log_warn!("Erreur lors du pin du message de présentation: {}", e);
        });

        Ok(new_channel.id)
    }
}