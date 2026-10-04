use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap},
    Frame,
};
use regex::Regex;

use crate::app::{AgentInfo, App, ConfirmAction, SidebarPanel};
use crate::config;
use crate::tmux::HealthState;

/// Dashboard theme palette. Two named slots — selected/active vs.
/// inactive/dim — referenced by every panel so the theme cannot drift
/// one hunk at a time via incidental edits (as happened between v0.1.2
/// and current main). Change these, change the whole dashboard.
const COLOR_ACTIVE: Color = Color::LightMagenta;
const COLOR_INACTIVE: Color = Color::DarkGray;

const QUOTES: &[&str] = &[
    // Sun Tzu
    "\"The art of war is of vital importance to the State.\" — Sun Tzu",
    "\"The supreme art of war is to subdue the enemy without fighting.\" — Sun Tzu",
    "\"Victorious warriors win first and then go to war, while defeated warriors go to war first and then seek to win.\" — Sun Tzu",
    "\"Appear weak when you are strong, and strong when you are weak.\" — Sun Tzu",
    "\"Let your plans be dark and impenetrable as night, and when you move, fall like a thunderbolt.\" — Sun Tzu",
    "\"In the midst of chaos, there is also opportunity.\" — Sun Tzu",
    "\"Know yourself and you will win all battles.\" — Sun Tzu",
    "\"The greatest victory is that which requires no battle.\" — Sun Tzu",
    "\"Opportunities multiply as they are seized.\" — Sun Tzu",
    "\"He who is prudent and lies in wait for an enemy who is not, will be victorious.\" — Sun Tzu",
    // Winston Churchill
    "\"To improve is to change; to be perfect is to change often.\" — Winston Churchill",
    "\"Success is not final, failure is not fatal: it is the courage to continue that counts.\" — Winston Churchill",
    "\"We shall fight on the beaches, we shall never surrender.\" — Winston Churchill",
    "\"If you're going through hell, keep going.\" — Winston Churchill",
    "\"The pessimist sees difficulty in every opportunity. The optimist sees opportunity in every difficulty.\" — Winston Churchill",
    "\"History will be kind to me for I intend to write it.\" — Winston Churchill",
    "\"Courage is what it takes to stand up and speak; courage is also what it takes to sit down and listen.\" — Winston Churchill",
    "\"You have enemies? Good. That means you've stood up for something, sometime in your life.\" — Winston Churchill",
    "\"Kites rise highest against the wind, not with it.\" — Winston Churchill",
    "\"Continuous effort — not strength or intelligence — is the key to unlocking our potential.\" — Winston Churchill",
    // Abraham Lincoln
    "\"Give me six hours to chop down a tree and I will spend the first four sharpening the axe.\" — Abraham Lincoln",
    "\"Nearly all men can stand adversity, but if you want to test a man's character, give him power.\" — Abraham Lincoln",
    "\"The best way to predict the future is to create it.\" — Abraham Lincoln",
    "\"I am a slow walker, but I never walk back.\" — Abraham Lincoln",
    "\"Whatever you are, be a good one.\" — Abraham Lincoln",
    "\"Those who deny freedom to others deserve it not for themselves.\" — Abraham Lincoln",
    "\"Do I not destroy my enemies when I make them my friends?\" — Abraham Lincoln",
    "\"The ballot is stronger than the bullet.\" — Abraham Lincoln",
    "\"In the end, it's not the years in your life that count. It's the life in your years.\" — Abraham Lincoln",
    "\"My great concern is not whether you have failed, but whether you are content with your failure.\" — Abraham Lincoln",
    // Mahatma Gandhi
    "\"Be the change you wish to see in the world.\" — Mahatma Gandhi",
    "\"An ounce of practice is worth more than tons of preaching.\" — Mahatma Gandhi",
    "\"The future depends on what you do today.\" — Mahatma Gandhi",
    "\"Strength does not come from physical capacity. It comes from an indomitable will.\" — Mahatma Gandhi",
    "\"In a gentle way, you can shake the world.\" — Mahatma Gandhi",
    "\"Live as if you were to die tomorrow. Learn as if you were to live forever.\" — Mahatma Gandhi",
    "\"First they ignore you, then they laugh at you, then they fight you, then you win.\" — Mahatma Gandhi",
    "\"The weak can never forgive. Forgiveness is the attribute of the strong.\" — Mahatma Gandhi",
    "\"You must not lose faith in humanity. Humanity is an ocean; if a few drops are dirty, the ocean does not become dirty.\" — Mahatma Gandhi",
    "\"A man is but the product of his thoughts. What he thinks, he becomes.\" — Mahatma Gandhi",
    // Martin Luther King Jr.
    "\"I have a dream that one day this nation will rise up.\" — Martin Luther King Jr.",
    "\"Injustice anywhere is a threat to justice everywhere.\" — Martin Luther King Jr.",
    "\"The time is always right to do what is right.\" — Martin Luther King Jr.",
    "\"Darkness cannot drive out darkness; only light can do that. Hate cannot drive out hate; only love can do that.\" — Martin Luther King Jr.",
    "\"Our lives begin to end the day we become silent about things that matter.\" — Martin Luther King Jr.",
    "\"Faith is taking the first step even when you don't see the whole staircase.\" — Martin Luther King Jr.",
    "\"The ultimate measure of a man is not where he stands in moments of comfort, but where he stands in times of challenge.\" — Martin Luther King Jr.",
    "\"Intelligence plus character — that is the goal of true education.\" — Martin Luther King Jr.",
    "\"Life's most persistent and urgent question is: What are you doing for others?\" — Martin Luther King Jr.",
    "\"If you can't fly then run, if you can't run then walk, if you can't walk then crawl, but whatever you do you have to keep moving forward.\" — Martin Luther King Jr.",
    // Nelson Mandela
    "\"Courage is not the absence of fear, but the triumph over it.\" — Nelson Mandela",
    "\"It always seems impossible until it's done.\" — Nelson Mandela",
    "\"Education is the most powerful weapon which you can use to change the world.\" — Nelson Mandela",
    "\"I learned that courage was not the absence of fear, but the triumph over it.\" — Nelson Mandela",
    "\"Do not judge me by my successes, judge me by how many times I fell down and got back up again.\" — Nelson Mandela",
    "\"A winner is a dreamer who never gives up.\" — Nelson Mandela",
    "\"After climbing a great hill, one only finds that there are many more hills to climb.\" — Nelson Mandela",
    "\"There is no passion to be found playing small — in settling for a life that is less than the one you are capable of living.\" — Nelson Mandela",
    "\"May your choices reflect your hopes, not your fears.\" — Nelson Mandela",
    "\"What counts in life is not the mere fact that we have lived. It is what difference we have made to the lives of others.\" — Nelson Mandela",
    // Albert Einstein
    "\"In the middle of difficulty lies opportunity.\" — Albert Einstein",
    "\"The measure of intelligence is the ability to change.\" — Albert Einstein",
    "\"Imagination is more important than knowledge.\" — Albert Einstein",
    "\"Life is like riding a bicycle. To keep your balance, you must keep moving.\" — Albert Einstein",
    "\"Strive not to be a success, but rather to be of value.\" — Albert Einstein",
    "\"The world as we have created it is a process of our thinking. It cannot be changed without changing our thinking.\" — Albert Einstein",
    "\"A person who never made a mistake never tried anything new.\" — Albert Einstein",
    "\"I have no special talents. I am only passionately curious.\" — Albert Einstein",
    "\"Logic will get you from A to Z; imagination will get you everywhere.\" — Albert Einstein",
    "\"Try not to become a man of success. Rather become a man of value.\" — Albert Einstein",
    // Confucius
    "\"It does not matter how slowly you go as long as you do not stop.\" — Confucius",
    "\"Our greatest glory is not in never falling, but in rising every time we fall.\" — Confucius",
    "\"He who knows all the answers has not been asked all the questions.\" — Confucius",
    "\"The man who moves a mountain begins by carrying away small stones.\" — Confucius",
    "\"Before you embark on a journey of revenge, dig two graves.\" — Confucius",
    "\"Real knowledge is to know the extent of one's ignorance.\" — Confucius",
    "\"Wheresoever you go, go with all your heart.\" — Confucius",
    "\"Everything has beauty, but not everyone sees it.\" — Confucius",
    "\"When it is obvious that the goals cannot be reached, don't adjust the goals, adjust the action steps.\" — Confucius",
    "\"The funniest people are the saddest ones.\" — Confucius",
    // Lao Tzu
    "\"Knowing others is intelligence; knowing yourself is true wisdom.\" — Lao Tzu",
    "\"A journey of a thousand miles begins with a single step.\" — Lao Tzu",
    "\"When I let go of what I am, I become what I might be.\" — Lao Tzu",
    "\"Nature does not hurry, yet everything is accomplished.\" — Lao Tzu",
    "\"The best fighter is never angry.\" — Lao Tzu",
    "\"Mastering others is strength. Mastering yourself is true power.\" — Lao Tzu",
    "\"He who conquers others is strong; he who conquers himself is mighty.\" — Lao Tzu",
    "\"Silence is a source of great strength.\" — Lao Tzu",
    "\"Be content with what you have; rejoice in the way things are.\" — Lao Tzu",
    "\"The wise man is one who knows what he does not know.\" — Lao Tzu",
    // Aristotle
    "\"We are what we repeatedly do. Excellence, then, is not an act, but a habit.\" — Aristotle",
    "\"Knowing yourself is the beginning of all wisdom.\" — Aristotle",
    "\"It is the mark of an educated mind to be able to entertain a thought without accepting it.\" — Aristotle",
    "\"Patience is bitter, but its fruit is sweet.\" — Aristotle",
    "\"The whole is greater than the sum of its parts.\" — Aristotle",
    "\"Quality is not an act, it is a habit.\" — Aristotle",
    "\"The roots of education are bitter, but the fruit is sweet.\" — Aristotle",
    "\"Pleasure in the job puts perfection in the work.\" — Aristotle",
    "\"The energy of the mind is the essence of life.\" — Aristotle",
    "\"Hope is a waking dream.\" — Aristotle",
    // Socrates
    "\"Let him who would move the world first move himself.\" — Socrates",
    "\"The unexamined life is not worth living.\" — Socrates",
    "\"I know that I know nothing.\" — Socrates",
    "\"To find yourself, think for yourself.\" — Socrates",
    "\"Strong minds discuss ideas, average minds discuss events, weak minds discuss people.\" — Socrates",
    "\"Be kind, for everyone you meet is fighting a hard battle.\" — Socrates",
    "\"Education is the kindling of a flame, not the filling of a vessel.\" — Socrates",
    "\"The secret of change is to focus all of your energy not on fighting the old, but on building the new.\" — Socrates",
    "\"Wonder is the beginning of wisdom.\" — Socrates",
    "\"He is richest who is content with the least, for contentment is the wealth of nature.\" — Socrates",
    // Marcus Aurelius
    "\"The happiness of your life depends upon the quality of your thoughts.\" — Marcus Aurelius",
    "\"You have power over your mind — not outside events. Realize this, and you will find strength.\" — Marcus Aurelius",
    "\"Waste no more time arguing about what a good man should be. Be one.\" — Marcus Aurelius",
    "\"The soul becomes dyed with the color of its thoughts.\" — Marcus Aurelius",
    "\"Very little is needed to make a happy life; it is all within yourself, in your way of thinking.\" — Marcus Aurelius",
    "\"When you arise in the morning, think of what a precious privilege it is to be alive.\" — Marcus Aurelius",
    "\"The best revenge is not to be like your enemy.\" — Marcus Aurelius",
    "\"It is not death that a man should fear, but he should fear never beginning to live.\" — Marcus Aurelius",
    "\"Accept the things to which fate binds you, and love the people with whom fate brings you together.\" — Marcus Aurelius",
    "\"Dwell on the beauty of life. Watch the stars, and see yourself running with them.\" — Marcus Aurelius",
    // Napoleon Bonaparte
    "\"A leader is a dealer in hope.\" — Napoleon Bonaparte",
    "\"Impossible is a word to be found only in the dictionary of fools.\" — Napoleon Bonaparte",
    "\"The world suffers a lot. Not because of the violence of bad people, but because of the silence of good people.\" — Napoleon Bonaparte",
    "\"Courage isn't having the strength to go on — it is going on when you don't have strength.\" — Napoleon Bonaparte",
    "\"Victory belongs to the most persevering.\" — Napoleon Bonaparte",
    "\"Take time to deliberate, but when the time for action comes, stop thinking and go in.\" — Napoleon Bonaparte",
    "\"Never interrupt your enemy when he is making a mistake.\" — Napoleon Bonaparte",
    "\"The battlefield is a scene of constant chaos. The winner will be the one who controls that chaos.\" — Napoleon Bonaparte",
    "\"In politics, stupidity is not a handicap.\" — Napoleon Bonaparte",
    "\"Great ambition is the passion of a great character.\" — Napoleon Bonaparte",
    // Alexander the Great
    "\"An army of sheep led by a lion is better than an army of lions led by a sheep.\" — Alexander the Great",
    "\"I am not afraid of an army of lions led by a sheep; I am afraid of an army of sheep led by a lion.\" — Alexander the Great",
    "\"There is nothing impossible to him who will try.\" — Alexander the Great",
    "\"I would rather live a short life of glory than a long one of obscurity.\" — Alexander the Great",
    "\"Remember, upon the conduct of each depends the fate of all.\" — Alexander the Great",
    // Franklin D. Roosevelt
    "\"The only thing we have to fear is fear itself.\" — Franklin D. Roosevelt",
    "\"The only limit to our realization of tomorrow will be our doubts of today.\" — Franklin D. Roosevelt",
    "\"In politics, nothing happens by accident. If it happens, you can bet it was planned that way.\" — Franklin D. Roosevelt",
    "\"A smooth sea never made a skilled sailor.\" — Franklin D. Roosevelt",
    "\"When you reach the end of your rope, tie a knot in it and hang on.\" — Franklin D. Roosevelt",
    // John F. Kennedy
    "\"Those who dare to fail miserably can achieve greatly.\" — John F. Kennedy",
    "\"Ask not what your country can do for you — ask what you can do for your country.\" — John F. Kennedy",
    "\"Change is the law of life. And those who look only to the past or present are certain to miss the future.\" — John F. Kennedy",
    "\"Efforts and courage are not enough without purpose and direction.\" — John F. Kennedy",
    "\"Leadership and learning are indispensable to each other.\" — John F. Kennedy",
    // Theodore Roosevelt
    "\"Do what you can, with what you have, where you are.\" — Theodore Roosevelt",
    "\"Believe you can and you're halfway there.\" — Theodore Roosevelt",
    "\"It is not the critic who counts; not the man who points out how the strong man stumbles.\" — Theodore Roosevelt",
    "\"In any moment of decision, the best thing you can do is the right thing.\" — Theodore Roosevelt",
    "\"Speak softly and carry a big stick; you will go far.\" — Theodore Roosevelt",
    // Ralph Waldo Emerson
    "\"What you do speaks so loudly that I cannot hear what you say.\" — Ralph Waldo Emerson",
    "\"The only person you are destined to become is the person you decide to be.\" — Ralph Waldo Emerson",
    "\"What lies behind us and what lies before us are tiny matters compared to what lies within us.\" — Ralph Waldo Emerson",
    "\"Do not go where the path may lead, go instead where there is no path and leave a trail.\" — Ralph Waldo Emerson",
    "\"To be yourself in a world that is constantly trying to make you something else is the greatest accomplishment.\" — Ralph Waldo Emerson",
    // Seneca
    "\"It is not that we have a short time to live, but that we waste a great deal of it.\" — Seneca",
    "\"Luck is what happens when preparation meets opportunity.\" — Seneca",
    "\"We suffer more often in imagination than in reality.\" — Seneca",
    "\"Difficulties strengthen the mind, as labor does the body.\" — Seneca",
    "\"He who is brave is free.\" — Seneca",
    // Leonardo da Vinci
    "\"Simplicity is the ultimate sophistication.\" — Leonardo da Vinci",
    "\"Learning never exhausts the mind.\" — Leonardo da Vinci",
    "\"It had long since come to my attention that people of accomplishment rarely sat back and let things happen to them.\" — Leonardo da Vinci",
    "\"I have been impressed with the urgency of doing. Knowing is not enough; we must apply.\" — Leonardo da Vinci",
    "\"The noblest pleasure is the joy of understanding.\" — Leonardo da Vinci",
    // Frederick Douglass
    "\"If there is no struggle, there is no progress.\" — Frederick Douglass",
    "\"It is easier to build strong children than to repair broken men.\" — Frederick Douglass",
    "\"Once you learn to read, you will be forever free.\" — Frederick Douglass",
    "\"Power concedes nothing without a demand. It never did and it never will.\" — Frederick Douglass",
    "\"I prayed for twenty years but received no answer until I prayed with my legs.\" — Frederick Douglass",
    // Harriet Tubman
    "\"Every great dream begins with a dreamer.\" — Harriet Tubman",
    "\"I freed a thousand slaves. I could have freed a thousand more if only they knew they were slaves.\" — Harriet Tubman",
    // Benjamin Franklin
    "\"An investment in knowledge pays the best interest.\" — Benjamin Franklin",
    "\"By failing to prepare, you are preparing to fail.\" — Benjamin Franklin",
    "\"Well done is better than well said.\" — Benjamin Franklin",
    "\"Energy and persistence conquer all things.\" — Benjamin Franklin",
    "\"Tell me and I forget, teach me and I may remember, involve me and I learn.\" — Benjamin Franklin",
    // George Washington
    "\"It is better to offer no excuse than a bad one.\" — George Washington",
    "\"Liberty, when it begins to take root, is a plant of rapid growth.\" — George Washington",
    "\"Discipline is the soul of an army.\" — George Washington",
    // Harry S. Truman
    "\"We must build a new world, a far better world.\" — Harry S. Truman",
    "\"It is amazing what you can accomplish if you do not care who gets the credit.\" — Harry S. Truman",
    // Dwight D. Eisenhower
    "\"Plans are nothing; planning is everything.\" — Dwight D. Eisenhower",
    "\"Leadership is the art of getting someone else to do something you want done because he wants to do it.\" — Dwight D. Eisenhower",
    "\"What counts is not necessarily the size of the dog in the fight — it's the size of the fight in the dog.\" — Dwight D. Eisenhower",
    // Epictetus
    "\"No man is free who is not master of himself.\" — Epictetus",
    "\"It's not what happens to you, but how you react to it that matters.\" — Epictetus",
    "\"First say to yourself what you would be; and then do what you have to do.\" — Epictetus",
    // Thucydides
    "\"The secret of happiness is freedom, and the secret of freedom is courage.\" — Thucydides",
    "\"The strong do what they can and the weak suffer what they must.\" — Thucydides",
    // Plato
    "\"The measure of a man is what he does with power.\" — Plato",
    "\"Wise men speak because they have something to say; fools because they have to say something.\" — Plato",
    "\"Be kind, for everyone you meet is fighting a harder battle.\" — Plato",
    // Steve Jobs
    "\"The only way to do great work is to love what you do.\" — Steve Jobs",
    "\"Stay hungry, stay foolish.\" — Steve Jobs",
    "\"Innovation distinguishes between a leader and a follower.\" — Steve Jobs",
    // Proverbs
    "\"The best time to plant a tree was 20 years ago. The second best time is now.\" — Chinese Proverb",
    "\"If you want to go fast, go alone. If you want to go far, go together.\" — African Proverb",
    "\"Fall seven times, stand up eight.\" — Japanese Proverb",
    "\"A smooth sea never made a skillful sailor.\" — English Proverb",
    "\"The bamboo that bends is stronger than the oak that resists.\" — Japanese Proverb",
    // Miyamoto Musashi
    "\"There is nothing outside of yourself that can ever enable you to get better, stronger, richer, quicker, or smarter.\" — Miyamoto Musashi",
    "\"Do nothing that is of no use.\" — Miyamoto Musashi",
    "\"Think lightly of yourself and deeply of the world.\" — Miyamoto Musashi",
    "\"The ultimate aim of martial arts is not having to use them.\" — Miyamoto Musashi",
    "\"You can only fight the way you practice.\" — Miyamoto Musashi",
    // Genghis Khan
    "\"If you're afraid — don't do it. If you're doing it — don't be afraid.\" — Genghis Khan",
    "\"An action committed in anger is an action doomed to failure.\" — Genghis Khan",
    "\"It is not sufficient that I succeed — all others must fail.\" — Genghis Khan",
    // Hannibal Barca
    "\"We will either find a way, or make one.\" — Hannibal Barca",
    // Julius Caesar
    "\"Experience is the teacher of all things.\" — Julius Caesar",
    "\"I came, I saw, I conquered.\" — Julius Caesar",
    "\"It is better to create than to learn! Creating is the essence of life.\" — Julius Caesar",
    // Queen Elizabeth I
    "\"I know I have the body of a weak and feeble woman, but I have the heart and stomach of a king.\" — Queen Elizabeth I",
    "\"I do not so much rejoice that God hath made me a queen, as to be a queen over so thankful a people.\" — Queen Elizabeth I",
    // Catherine the Great
    "\"I shall be an autocrat: that's my trade. And the good Lord will forgive me: that's his.\" — Catherine the Great",
    // Harriet Beecher Stowe
    "\"Never give up, for that is just the place and time that the tide will turn.\" — Harriet Beecher Stowe",
    // Eleanor Roosevelt
    "\"No one can make you feel inferior without your consent.\" — Eleanor Roosevelt",
    "\"The future belongs to those who believe in the beauty of their dreams.\" — Eleanor Roosevelt",
    "\"Do one thing every day that scares you.\" — Eleanor Roosevelt",
    "\"You gain strength, courage, and confidence by every experience in which you really stop to look fear in the face.\" — Eleanor Roosevelt",
    // Maya Angelou
    "\"If you don't like something, change it. If you can't change it, change your attitude.\" — Maya Angelou",
    "\"We delight in the beauty of the butterfly, but rarely admit the changes it has gone through to achieve that beauty.\" — Maya Angelou",
    "\"There is no greater agony than bearing an untold story inside you.\" — Maya Angelou",
    // Voltaire
    "\"Judge a man by his questions rather than by his answers.\" — Voltaire",
    "\"The more I read, the more I acquire, the more certain I am that I know nothing.\" — Voltaire",
    // Victor Hugo
    "\"Even the darkest night will end and the sun will rise.\" — Victor Hugo",
    "\"He who opens a school door, closes a prison.\" — Victor Hugo",
    // Cleopatra
    "\"I will not be triumphed over.\" — Cleopatra",
    // Joan of Arc
    "\"I am not afraid. I was born to do this.\" — Joan of Arc",
    "\"Act, and God will act.\" — Joan of Arc",
    // Khalil Gibran
    "\"Out of suffering have emerged the strongest souls; the most massive characters are seared with scars.\" — Khalil Gibran",
    "\"Your living is determined not so much by what life brings to you as by the attitude you bring to life.\" — Khalil Gibran",
    // Cicero
    "\"The life of the dead is placed in the memory of the living.\" — Cicero",
    "\"A room without books is like a body without a soul.\" — Cicero",
    "\"Gratitude is not only the greatest of virtues, but the parent of all others.\" — Cicero",
    // Heraclitus
    "\"No man ever steps in the same river twice, for it is not the same river and he is not the same man.\" — Heraclitus",
    "\"The only constant in life is change.\" — Heraclitus",
    // Tacitus
    "\"The desire for safety stands against every great and noble enterprise.\" — Tacitus",
    // Pericles
    "\"What you leave behind is not what is engraved in stone monuments, but what is woven into the lives of others.\" — Pericles",
    "\"Freedom is the sure possession of those alone who have the courage to defend it.\" — Pericles",
    // Simón Bolívar
    "\"A people that loves freedom will in the end be free.\" — Simón Bolívar",
    "\"The art of victory is learned in defeat.\" — Simón Bolívar",
    // Marie Curie
    "\"Nothing in life is to be feared, it is only to be understood.\" — Marie Curie",
    "\"One never notices what has been done; one can only see what remains to be done.\" — Marie Curie",
    // Rumi
    "\"The wound is the place where the Light enters you.\" — Rumi",
    "\"Yesterday I was clever, so I wanted to change the world. Today I am wise, so I am changing myself.\" — Rumi",
    // Fyodor Dostoevsky
    "\"The soul is healed by being with children.\" — Fyodor Dostoevsky",
    "\"Pain and suffering are always inevitable for a large intelligence and a deep heart.\" — Fyodor Dostoevsky",
    // Leo Tolstoy
    "\"Everyone thinks of changing the world, but no one thinks of changing himself.\" — Leo Tolstoy",
    // Prophetic Tradition
    "\"The leader of a people is their servant.\" — Prophetic Tradition",
    "\"All of you are shepherds and each of you is responsible for his flock.\" — Prophetic Tradition",
];

pub const QUOTE_COUNT: usize = QUOTES.len();

/// Render the entire dashboard
pub fn render(frame: &mut Frame, app: &App) {
    let status_height = 3;
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(status_height), // Status bar
            Constraint::Min(8),                // Main content area
            Constraint::Length(1),             // Help bar
        ])
        .split(frame.area());

    render_status_bar(frame, app, outer[0]);

    // Two-column layout: sidebar + main content (sidebar can be left or right)
    let columns = if app.config.dashboard.sidebar_right {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(40)])
            .split(outer[1]);
        (cols[1], cols[0]) // (sidebar, main)
    } else {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(40), Constraint::Min(0)])
            .split(outer[1]);
        (cols[0], cols[1]) // (sidebar, main)
    };
    let (sidebar_area, main_area) = columns;

    // Sidebar: projects, (optional) event queue, chain of command
    if app.config.dashboard.show_event_queue {
        let sidebar = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
                Constraint::Ratio(1, 3),
            ])
            .split(sidebar_area);

        render_projects_panel(frame, app, sidebar[0]);
        render_event_queue(frame, app, sidebar[1]);
        render_command_tree(frame, app, sidebar[2]);
    } else {
        let sidebar = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
            .split(sidebar_area);

        render_projects_panel(frame, app, sidebar[0]);
        render_command_tree(frame, app, sidebar[1]);
    }

    // Main area: agent grid on top (~2/3), focus parent on bottom (~1/3)
    let main_col = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(67), Constraint::Min(8)])
        .split(main_area);

    render_agent_grid(frame, app, main_col[0]);
    render_focus_parent(frame, app, main_col[1]);

    render_help_bar(frame, app, outer[2]);

    // Render overlays
    if app.show_help {
        render_help_popup(frame, app);
    }

    if let Some(action) = app.pending_confirm {
        render_confirm_dialog(frame, app, action);
    }

    if app.project_input_mode {
        render_project_input(frame, app);
    }

    if app.ea_input_mode {
        render_ea_input(frame, app);
    }

    if app.show_events {
        render_events_popup(frame, app);
    }

    if app.show_debug_console {
        render_debug_console(frame, app);
    }

    if app.show_settings {
        render_settings_popup(frame, app);
    }

    if let Some(panel) = app.sidebar_popup {
        render_sidebar_popup(frame, app, panel);
    }
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let (running, idle) = app.health_counts();
    let total = app.total_agents();

    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;

    let heading = match &app.session {
        Some(session) => format!("OMAR {} ", session.name),
        None => "OMAR ".to_string(),
    };
    let mut status_spans = vec![
        Span::styled(heading, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("| Agents: "),
        Span::styled(format!("{}", total), Style::default().fg(Color::Reset)),
        Span::raw(" | "),
        Span::styled(
            format!("{} Running", running),
            Style::default().fg(Color::Green),
        ),
        Span::raw(" "),
        Span::styled(format!("{} Idle", idle), Style::default().fg(Color::Yellow)),
    ];

    // Events count
    if !app.scheduled_events.is_empty() {
        status_spans.push(Span::raw(" | Events: "));
        status_spans.push(Span::styled(
            format!("{}", app.scheduled_events.len()),
            Style::default().fg(COLOR_ACTIVE),
        ));
    }

    // EA Wake countdown
    let next_ea_event = app
        .scheduled_events
        .iter()
        .filter(|e| e.receiver == "ea")
        .min_by_key(|e| e.timestamp);
    if let Some(event) = next_ea_event {
        status_spans.push(Span::raw(" | EA Wake: "));
        status_spans.push(Span::styled(
            format_countdown_ns(event.timestamp, now_ns),
            Style::default().fg(COLOR_ACTIVE),
        ));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(COLOR_INACTIVE))
        .padding(Padding::horizontal(1));

    // Render block first, then split inner area
    let inner = block.inner(area);
    frame.render_widget(block, area);

    render_status_row(frame, app, &status_spans, inner);
}

/// Render the status info on the left and a scrolling quote on the right.
fn render_status_row(frame: &mut Frame, app: &App, status_spans: &[Span], area: Rect) {
    let left_width: u16 = status_spans.iter().map(|s| s.width() as u16).sum();
    let left_col_width = left_width.saturating_add(1).min(area.width);

    let h_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(left_col_width), Constraint::Min(1)])
        .split(area);

    // Left: stats
    let stats = Paragraph::new(Line::from(status_spans.to_vec()));
    frame.render_widget(stats, h_chunks[0]);

    // Right: scrolling quote (if enabled in settings)
    let quote_width = h_chunks[1].width as usize;
    if app.config.dashboard.show_quotes && quote_width > 5 {
        let qi = app.quote_order[app.quote_index % app.quote_order.len()];
        let quote = QUOTES[qi];
        let quote_len = quote.chars().count();

        let visible: String = if quote_len <= quote_width {
            format!("{:>width$}", quote, width = quote_width)
        } else {
            let padded: String = std::iter::repeat_n(' ', quote_width)
                .chain(quote.chars())
                .collect();
            let total_len = padded.chars().count();
            let offset = app.ticker_offset % total_len;
            padded
                .chars()
                .cycle()
                .skip(offset)
                .take(quote_width)
                .collect()
        };

        let quote_paragraph = Paragraph::new(Line::from(Span::styled(
            visible,
            Style::default().fg(COLOR_INACTIVE),
        )));
        frame.render_widget(quote_paragraph, h_chunks[1]);
    }
}

fn render_projects_panel(frame: &mut Frame, app: &App, area: Rect) {
    let panel_active = app.sidebar_focused && app.sidebar_panel == SidebarPanel::Projects;
    let border_color = if panel_active {
        COLOR_ACTIVE
    } else {
        COLOR_INACTIVE
    };
    let block = Block::default()
        .title(" Projects ")
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(border_color))
        .padding(Padding::horizontal(1));

    if app.projects.is_empty() {
        let paragraph = Paragraph::new(Span::styled(
            "No active projects. Spawn a project by chatting with the executive assistant.",
            Style::default().fg(COLOR_INACTIVE),
        ))
        .block(block)
        .wrap(Wrap { trim: true });
        frame.render_widget(paragraph, area);
        return;
    }

    let lines: Vec<Line> = app
        .projects
        .iter()
        .map(|p| {
            Line::from(Span::styled(
                format!("{}. {}", p.id, p.name),
                Style::default().fg(Color::Reset),
            ))
        })
        .collect();

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });

    frame.render_widget(paragraph, area);
}

fn render_agent_grid(frame: &mut Frame, app: &App, area: Rect) {
    let children = app.focus_children();

    if children.is_empty() {
        let empty_msg = Paragraph::new("Chat with the executive assistant to spawn agents.")
            .style(Style::default().fg(COLOR_INACTIVE))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Thick)
                    .title(" Agents ")
                    .border_style(Style::default().fg(COLOR_INACTIVE))
                    .padding(Padding::horizontal(1)),
            );
        frame.render_widget(empty_msg, area);
        return;
    }

    // Simple 2-column grid layout
    let cols = 2.min(children.len()).max(1);
    let total_rows = children.len().div_ceil(cols);

    let row_height = (area.height / total_rows as u16).max(6);
    let row_constraints: Vec<Constraint> = (0..total_rows)
        .map(|_| Constraint::Length(row_height))
        .collect();

    let row_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(area);

    for (i, child) in children.iter().enumerate() {
        let row = i / cols;
        let col = i % cols;

        if row >= row_chunks.len() {
            break;
        }

        let col_constraints: Vec<Constraint> = (0..cols)
            .map(|_| Constraint::Ratio(1, cols as u32))
            .collect();

        let col_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(col_constraints)
            .split(row_chunks[row]);

        if col < col_chunks.len() {
            let is_selected = !app.sidebar_focused && !app.manager_selected && i == app.selected;
            render_summary_card(frame, app, child, col_chunks[col], is_selected);
        }
    }
}

fn render_focus_parent(frame: &mut Frame, app: &App, area: Rect) {
    let parent_info = app.focus_parent_info();

    if let Some(info) = parent_info {
        let is_selected = app.manager_selected && !app.sidebar_focused;

        // Build display title based on focus parent type
        let parent_name = &app.focus_parent;
        let is_manager = app
            .manager
            .as_ref()
            .map(|m| m.session.name == *parent_name)
            .unwrap_or(false);
        let display_title = if is_manager {
            // Show which EA this is
            let ea_name = app
                .registered_eas
                .iter()
                .find(|ea| ea.id == app.active_ea)
                .map(|ea| ea.name.as_str())
                .unwrap_or("EA");
            format!("Executive Assistant ({})", ea_name)
        } else {
            let short = parent_name
                .strip_prefix(app.client().prefix())
                .unwrap_or(parent_name);
            let mut short = short.to_string();
            if parent_info.map(|p| p.is_unresolved).unwrap_or(false) {
                short.push(' ');
                short.push_str("[unresolved]");
            }
            short
        };

        // Health status dot
        let (health_color, status_icon) = match info.health {
            HealthState::Running => (Color::Green, "●"),
            HealthState::Idle => (Color::Yellow, "○"),
        };

        let (border_color, title_line) = if is_selected {
            (
                COLOR_ACTIVE,
                Line::from(vec![
                    Span::styled(" [", Style::default().fg(COLOR_ACTIVE)),
                    Span::styled(status_icon, Style::default().fg(COLOR_ACTIVE)),
                    Span::styled("] ", Style::default().fg(COLOR_ACTIVE)),
                    Span::styled(&display_title, Style::default().fg(COLOR_ACTIVE)),
                    Span::styled(" - Enter to open ", Style::default().fg(COLOR_ACTIVE)),
                ]),
            )
        } else {
            (
                COLOR_INACTIVE,
                Line::from(vec![
                    Span::styled(" ", Style::default().fg(COLOR_INACTIVE)),
                    Span::styled(status_icon, Style::default().fg(health_color)),
                    Span::styled(" ", Style::default().fg(COLOR_INACTIVE)),
                    Span::styled(&display_title, Style::default().fg(health_color)),
                    Span::styled(" ", Style::default().fg(COLOR_INACTIVE)),
                ]),
            )
        };

        let border_style = Style::default()
            .fg(border_color)
            .add_modifier(if is_selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });

        let block = Block::default()
            .title(title_line)
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(border_style)
            .padding(Padding::horizontal(1));

        // Get focus parent output - more lines to fill the panel
        let available_lines = area.height.saturating_sub(2) as i32;
        let output = app
            .get_focus_parent_output(available_lines.max(20))
            .unwrap_or_default();

        // Parse ANSI codes and convert to ratatui text
        let mut content = match ansi_to_tui::IntoText::into_text(&output) {
            Ok(text) => text,
            Err(_) => {
                let plain = strip_ansi(&output);
                ratatui::text::Text::raw(plain)
            }
        };

        if !is_manager && app.child_count(&app.focus_parent) > 0 {
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
            let workers = app.focus_children();
            let running_workers = workers
                .iter()
                .filter(|a| matches!(a.health, HealthState::Running))
                .count();
            let next_pm_wake = app
                .scheduled_events
                .iter()
                .filter(|e| e.receiver == app.focus_parent)
                .min_by_key(|e| e.timestamp);

            let indicator = if let Some(event) = next_pm_wake {
                Line::from(vec![
                    Span::styled("PM Wake: ", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        format_countdown_ns(event.timestamp, now_ns),
                        Style::default().fg(COLOR_ACTIVE),
                    ),
                    Span::raw(" | "),
                    Span::styled(
                        format!("Workers running: {}", running_workers),
                        Style::default().fg(Color::Green),
                    ),
                    Span::raw(" | "),
                    Span::styled("ETA unknown", Style::default().fg(COLOR_INACTIVE)),
                ])
            } else if !workers.is_empty() {
                Line::from(vec![
                    Span::styled("PM Wake: ", Style::default().fg(Color::Cyan)),
                    Span::styled("not scheduled", Style::default().fg(Color::Yellow)),
                    Span::raw(" | "),
                    Span::styled(
                        format!("Workers running: {}", running_workers),
                        Style::default().fg(Color::Green),
                    ),
                    Span::raw(" | "),
                    Span::styled("ETA unknown", Style::default().fg(COLOR_INACTIVE)),
                ])
            } else {
                Line::from(vec![
                    Span::styled("PM status: ", Style::default().fg(Color::Cyan)),
                    Span::styled("no workers", Style::default().fg(COLOR_INACTIVE)),
                ])
            };

            content.lines.insert(0, indicator);
            content.lines.insert(1, Line::from(""));
        }

        // Calculate scroll to show bottom of content
        let content_height = content.lines.len() as u16;
        let visible_height = area.height.saturating_sub(2);
        let scroll = content_height.saturating_sub(visible_height);

        let paragraph = Paragraph::new(content)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));

        frame.render_widget(paragraph, area);
    } else {
        let block = Block::default()
            .title(" Executive Assistant ")
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(COLOR_INACTIVE))
            .padding(Padding::horizontal(1));

        let message = "Starting Executive Assistant...".to_string();

        let paragraph = Paragraph::new(message)
            .style(Style::default().fg(COLOR_INACTIVE))
            .block(block);

        frame.render_widget(paragraph, area);
    }
}

fn render_command_tree(frame: &mut Frame, app: &App, area: Rect) {
    let panel_active = app.sidebar_focused && app.sidebar_panel == SidebarPanel::ChainOfCommand;
    let border_color = if panel_active {
        COLOR_ACTIVE
    } else {
        COLOR_INACTIVE
    };
    let block = Block::default()
        .title(" Chain of Command ")
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(border_color))
        .padding(Padding::horizontal(1));

    if app.command_tree.is_empty() {
        let paragraph = Paragraph::new(Span::styled(
            "No agents yet.",
            Style::default().fg(COLOR_INACTIVE),
        ))
        .block(block);
        frame.render_widget(paragraph, area);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    for node in &app.command_tree {
        let (health_color, icon) = match node.health {
            HealthState::Running => (Color::Green, "●"),
            HealthState::Idle => (Color::Yellow, "○"),
        };

        // Check if this node is the current focus parent
        let is_focus = node.session_name == app.focus_parent;

        let mut spans: Vec<Span> = Vec::new();

        if node.depth == 0 {
            // Root (EA): no connector, just name + icon
            let name_style = if is_focus {
                Style::default()
                    .fg(COLOR_ACTIVE)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::Reset)
                    .add_modifier(Modifier::BOLD)
            };
            if is_focus {
                spans.push(Span::styled("►", Style::default().fg(COLOR_ACTIVE)));
            }
            spans.push(Span::styled(format!(" {} ", node.name), name_style));
            spans.push(Span::styled(icon, Style::default().fg(health_color)));
        } else {
            // Build prefix from ancestor continuation lines
            let mut prefix = String::from(" ");
            for i in 0..node.ancestor_is_last.len() {
                if i == 0 {
                    continue; // skip EA level (always root)
                }
                if node.ancestor_is_last[i] {
                    prefix.push_str("    ");
                } else {
                    prefix.push_str(" │  ");
                }
            }

            // Add connector for this node
            if node.is_last_sibling {
                prefix.push_str(" └── ");
            } else {
                prefix.push_str(" ├── ");
            }

            spans.push(Span::styled(prefix, Style::default().fg(COLOR_INACTIVE)));

            let name_style = if is_focus {
                Style::default()
                    .fg(COLOR_ACTIVE)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Reset)
            };

            if is_focus {
                spans.push(Span::styled("►", Style::default().fg(COLOR_ACTIVE)));
            }
            let mut node_name = node.name.clone();
            if node.is_unresolved {
                node_name.push(' ');
                node_name.push_str("[unresolved]");
            }
            spans.push(Span::styled(format!("{} ", node_name), name_style));
            spans.push(Span::styled(icon, Style::default().fg(health_color)));
        }

        lines.push(Line::from(spans));
    }

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_event_queue(frame: &mut Frame, app: &App, area: Rect) {
    let panel_active = app.sidebar_focused && app.sidebar_panel == SidebarPanel::Events;
    let border_color = if panel_active {
        COLOR_ACTIVE
    } else {
        COLOR_INACTIVE
    };
    let block = Block::default()
        .title(" Event Queue ")
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(border_color))
        .padding(Padding::horizontal(1));

    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;

    let mut lines: Vec<Line> = Vec::new();

    // Event list
    let available = area.height.saturating_sub(2) as usize; // borders
    let list_budget = available;

    if !app.scheduled_events.is_empty() {
        for event in app.scheduled_events.iter().take(list_budget) {
            let countdown = format_countdown_ns(event.timestamp, now_ns);
            let receiver = truncate_str(&event.receiver, 10);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<11}", receiver),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(countdown, Style::default().fg(COLOR_ACTIVE)),
            ]));
        }

        let remaining = app.scheduled_events.len().saturating_sub(list_budget);
        if remaining > 0 && lines.len() < available {
            lines.push(Line::from(Span::styled(
                format!("+{} more", remaining),
                Style::default().fg(COLOR_INACTIVE),
            )));
        }
    }

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

/// Strip ANSI escape codes from a string (fallback)
fn strip_ansi(s: &str) -> String {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap());
    re.replace_all(s, "").to_string()
}

fn render_summary_card(
    frame: &mut Frame,
    app: &App,
    agent: &AgentInfo,
    area: Rect,
    selected: bool,
) {
    let (health_color, status_icon) = match agent.health {
        HealthState::Running => (Color::Green, "●"),
        HealthState::Idle => (Color::Yellow, "○"),
    };

    let border_color = if selected {
        COLOR_ACTIVE
    } else {
        COLOR_INACTIVE
    };

    let border_style = Style::default().fg(border_color).add_modifier(if selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    });

    // Display name: strip session prefix
    let short_name = agent
        .session
        .name
        .strip_prefix(app.client().prefix())
        .unwrap_or(&agent.session.name);
    let mut title_name = short_name.to_string();
    if agent.is_unresolved {
        title_name.push(' ');
        title_name.push_str("[unresolved]");
    }

    // Title with status indicator
    let title_line = if selected {
        Line::from(vec![
            Span::styled(" [", Style::default().fg(COLOR_ACTIVE)),
            Span::styled(status_icon, Style::default().fg(COLOR_ACTIVE)),
            Span::styled("] ", Style::default().fg(COLOR_ACTIVE)),
            Span::styled(&title_name, Style::default().fg(COLOR_ACTIVE)),
            Span::styled(" ", Style::default().fg(COLOR_ACTIVE)),
        ])
    } else {
        Line::from(vec![
            Span::styled(" ", Style::default().fg(border_color)),
            Span::styled(status_icon, Style::default().fg(health_color)),
            Span::styled(" ", Style::default().fg(border_color)),
            Span::styled(&title_name, Style::default().fg(health_color)),
            Span::styled(" ", Style::default().fg(border_color)),
        ])
    };

    let block = Block::default()
        .title(title_line)
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(border_style)
        .padding(Padding::horizontal(1));

    // Available width for text content (minus borders and horizontal padding)
    let content_width = area.width.saturating_sub(4) as usize; // -2 borders, -2 padding

    let mut lines: Vec<Line> = Vec::new();

    // Sub-agent count (first)
    let child_count = app.child_count(&agent.session.name);
    if child_count > 0 {
        lines.push(Line::from(vec![
            Span::styled("▶ ", Style::default().fg(Color::Cyan)),
            Span::styled(
                format!("{} workers", child_count),
                Style::default().fg(Color::Reset),
            ),
            Span::styled(
                " (Tab to drill in, Shift-Tab to back out)",
                Style::default().fg(COLOR_INACTIVE),
            ),
        ]));
    }

    // Task (multi-line word wrap to fill available card space)
    let task = app
        .worker_tasks()
        .get(&agent.session.name)
        .cloned()
        .unwrap_or_else(|| "(no task assigned)".to_string());
    let label = "Task: ";
    let inner_height = area.height.saturating_sub(2) as usize; // minus top/bottom border
    let task_lines_budget = inner_height.saturating_sub(lines.len());

    if task_lines_budget > 0 {
        // Word-wrap the task text into lines
        let first_width = content_width.saturating_sub(label.len());
        let cont_width = content_width;

        let mut wrapped: Vec<String> = Vec::new();
        let mut remaining = task.as_str();

        // First line has reduced width due to "Task: " label
        let widths = std::iter::once(first_width).chain(std::iter::repeat(cont_width));
        for w in widths {
            if remaining.is_empty() || w == 0 {
                break;
            }
            if remaining.chars().count() <= w {
                wrapped.push(remaining.to_string());
                remaining = "";
            } else {
                // Find the byte index corresponding to `w` characters
                let byte_w = remaining
                    .char_indices()
                    .nth(w)
                    .map(|(i, _)| i)
                    .unwrap_or(remaining.len());
                // Try to break at a word boundary within the first `w` chars
                let break_at = remaining[..byte_w]
                    .rfind(' ')
                    .map(|i| i + 1) // include the space on the current line
                    .unwrap_or(byte_w);
                wrapped.push(remaining[..break_at].trim_end().to_string());
                remaining = &remaining[break_at..];
            }
        }

        // Fit into budget, truncating last visible line if needed
        if wrapped.len() <= task_lines_budget {
            // Everything fits
            for (i, line_text) in wrapped.iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled(label, Style::default().fg(Color::Reset)),
                        Span::styled(line_text.clone(), Style::default().fg(Color::Reset)),
                    ]));
                } else {
                    lines.push(Line::from(Span::styled(
                        line_text.clone(),
                        Style::default().fg(Color::Reset),
                    )));
                }
            }
        } else {
            // Truncate: show budget-1 full lines + last line with "..."
            let last = task_lines_budget - 1;
            for (i, text) in wrapped.iter().enumerate().take(task_lines_budget) {
                let is_first = i == 0;
                if i < last {
                    if is_first {
                        lines.push(Line::from(vec![
                            Span::styled(label, Style::default().fg(Color::Reset)),
                            Span::styled(text.clone(), Style::default().fg(Color::Reset)),
                        ]));
                    } else {
                        lines.push(Line::from(Span::styled(
                            text.clone(),
                            Style::default().fg(Color::Reset),
                        )));
                    }
                } else {
                    // Last line: truncate with ellipsis
                    let w = if is_first { first_width } else { cont_width };
                    let truncated = truncate_str(text, w);
                    if is_first {
                        lines.push(Line::from(vec![
                            Span::styled(label, Style::default().fg(Color::Reset)),
                            Span::styled(truncated, Style::default().fg(Color::Reset)),
                        ]));
                    } else {
                        lines.push(Line::from(Span::styled(
                            truncated,
                            Style::default().fg(Color::Reset),
                        )));
                    }
                }
            }
        }
    }

    let paragraph = Paragraph::new(lines).block(block);

    frame.render_widget(paragraph, area);
}

fn render_help_bar(frame: &mut Frame, app: &App, area: Rect) {
    let at_root = app
        .manager
        .as_ref()
        .map(|m| app.focus_parent == m.session.name)
        .unwrap_or(true);

    let mut help_text = vec![
        Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Chat "),
        Span::styled("Tab", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Drill-in "),
        Span::styled("n/N", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":New "),
        Span::styled("d/D", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Kill | "),
        Span::styled("z", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Hold the line | "),
        Span::styled("Q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(":Quit | "),
    ];
    if !at_root {
        help_text.push(Span::styled(
            "Esc",
            Style::default().add_modifier(Modifier::BOLD),
        ));
        help_text.push(Span::raw(":Back | "));
    }
    help_text.push(Span::styled(
        "S",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    help_text.push(Span::raw(":Settings "));
    help_text.push(Span::styled(
        "?",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    help_text.push(Span::raw(":Help"));

    // Right side: status message takes priority over ticker
    let right_content = if let Some(ref msg) = app.status_message {
        Some((msg.clone(), Style::default().fg(Color::Cyan)))
    } else {
        let ticker_content = app.ticker.render(std::time::Duration::from_secs(5));
        if !ticker_content.is_empty() {
            Some((ticker_content, Style::default().fg(Color::Yellow)))
        } else {
            None
        }
    };

    if let Some((right_text, right_style)) = right_content {
        // Compute help text width (sum of span widths)
        let help_width: u16 = help_text.iter().map(|s| s.width() as u16).sum();
        let help_col_width = help_width.saturating_add(1).min(area.width);

        let h_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(help_col_width), Constraint::Min(1)])
            .split(area);

        // Left: help text
        let help_paragraph =
            Paragraph::new(Line::from(help_text)).style(Style::default().fg(COLOR_INACTIVE));
        frame.render_widget(help_paragraph, h_chunks[0]);

        // Right: status message or ticker
        let right_area_width = h_chunks[1].width as usize;
        if right_area_width > 0 {
            let content_len = right_text.chars().count();
            let visible = if content_len <= right_area_width {
                format!("{:>width$}", right_text, width = right_area_width)
            } else {
                // Too long — scroll
                let padded: String = std::iter::repeat_n(' ', right_area_width)
                    .chain(right_text.chars())
                    .collect();
                let total_len = padded.chars().count();
                let offset = app.ticker_offset % total_len;
                padded
                    .chars()
                    .cycle()
                    .skip(offset)
                    .take(right_area_width)
                    .collect()
            };

            let right_paragraph = Paragraph::new(Line::from(Span::styled(visible, right_style)));
            frame.render_widget(right_paragraph, h_chunks[1]);
        }
    } else {
        // No right content — full-width help text
        let paragraph =
            Paragraph::new(Line::from(help_text)).style(Style::default().fg(COLOR_INACTIVE));
        frame.render_widget(paragraph, area);
    }
}

fn render_help_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(60, 50, frame.area());

    let help_content = vec![
        Line::from(Span::styled(
            "Keyboard Shortcuts",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(if app.session.is_some() {
            "  Q           Stop this session's runtime (z detaches instead)"
        } else {
            "  Q           Quit and reset runtime state"
        }),
        Line::from("  ←/→, h/l   Switch panel (sidebar ↔ main)"),
        Line::from("  ↑/↓, j/k   Move selection up/down"),
        Line::from("  Tab         Drill into selected agent"),
        Line::from("  Shift+Tab   Back (drill up)"),
        Line::from("  Esc         Back (drill up)"),
        Line::from("  Enter       Attach to selected agent"),
        Line::from("  n           Spawn new agent"),
        Line::from("  d           Kill selected agent"),
        Line::from("  N           Spawn new EA (prompts for name)"),
        Line::from("  D           Delete current EA (not the only one)"),
        Line::from("  p           Add a project"),
        Line::from("  [           Previous EA"),
        Line::from("  ]           Next EA"),
        Line::from("  e           Show scheduled events"),
        Line::from("  G           Debug console"),
        Line::from("  S           Settings"),
        Line::from("  z           Detach (dashboard keeps running)"),
        Line::from("  r           Refresh agent list"),
        Line::from("  ?           Toggle this help"),
        Line::from(""),
        Line::from(Span::styled(
            "Press any key to close",
            Style::default().fg(COLOR_INACTIVE),
        )),
    ];

    let block = Block::default()
        .title(" Help ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(help_content).block(block);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_confirm_dialog(frame: &mut Frame, app: &App, action: ConfirmAction) {
    let (title, heading, detail, hint, width) = match action {
        ConfirmAction::Kill => {
            let name = app
                .selected_agent()
                .map(|a| a.session.name.clone())
                .unwrap_or_else(|| "?".to_string());
            (" Confirm ", "Kill this agent?", name, String::new(), 40)
        }
        ConfirmAction::ResetQuit => match &app.session {
            Some(session) => (
                " Confirm Quit ",
                "Stop this session?",
                format!(
                    "Session {} shuts down: every EA, agent, and topology.",
                    session.name
                ),
                "Press z to detach instead.".to_string(),
                60,
            ),
            None => (
                " Confirm Quit ",
                "Quit omar?",
                "This will kill ALL EA sessions and agents.".to_string(),
                "Press z to walk away instead.".to_string(),
                50,
            ),
        },
        ConfirmAction::DeleteEa => {
            let ea_name = app
                .registered_eas
                .iter()
                .find(|ea| ea.id == app.active_ea)
                .map(|ea| ea.name.clone())
                .unwrap_or_else(|| "Unknown EA".to_string());
            (
                " Confirm Delete EA ",
                "Delete this EA?",
                ea_name,
                "This will kill all agents and remove all state.".to_string(),
                55,
            )
        }
    };

    let area = centered_rect(width, 30, frame.area());

    let mut content = vec![
        Line::from(""),
        Line::from(Span::styled(
            heading,
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(detail, Style::default().fg(Color::Yellow))),
    ];
    if !hint.is_empty() {
        content.push(Line::from(Span::styled(
            hint,
            Style::default().fg(COLOR_INACTIVE),
        )));
    }
    content.push(Line::from(""));
    content.push(Line::from(vec![
        Span::styled("y", Style::default().fg(Color::Green)),
        Span::raw(": Yes  "),
        Span::styled("n", Style::default().fg(Color::Red)),
        Span::raw(": No"),
    ]));

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));

    let paragraph = Paragraph::new(content)
        .block(block)
        .alignment(ratatui::layout::Alignment::Center);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_project_input(frame: &mut Frame, app: &App) {
    let area = centered_rect(50, 20, frame.area());

    let content = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Add Project",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("> {}_", app.project_input),
            Style::default().fg(Color::Cyan),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Enter to confirm, Esc to cancel",
            Style::default().fg(COLOR_INACTIVE),
        )),
    ];

    let block = Block::default()
        .title(" New Project ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(content)
        .block(block)
        .alignment(ratatui::layout::Alignment::Center);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_ea_input(frame: &mut Frame, app: &App) {
    let area = centered_rect(50, 20, frame.area());

    let content = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Create New EA",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("> {}_", app.ea_input),
            Style::default().fg(Color::Cyan),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Enter to confirm, Esc to cancel",
            Style::default().fg(COLOR_INACTIVE),
        )),
    ];

    let block = Block::default()
        .title(" New EA ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(content)
        .block(block)
        .alignment(ratatui::layout::Alignment::Center);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_events_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(70, 60, frame.area());
    let inner_width = area.width.saturating_sub(2) as usize; // borders
    let fixed_cols: usize = 14 + 14 + 16 + 14; // Sender + Receiver + Fires in + Type
    let payload_width = inner_width.saturating_sub(fixed_cols + 1);

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "Scheduled Events",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    if app.scheduled_events.is_empty() {
        lines.push(Line::from(Span::styled(
            "No events in queue",
            Style::default().fg(COLOR_INACTIVE),
        )));
    } else {
        // Header
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "{:<14} {:<14} {:<16} {:<14} ",
                    "Sender", "Receiver", "Fires in", "Type"
                ),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Payload",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            "─".repeat(inner_width),
            Style::default().fg(COLOR_INACTIVE),
        )));

        for event in &app.scheduled_events {
            // Format timestamp as human-readable relative time
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;
            let time_str = if event.timestamp <= now_ns {
                "overdue".to_string()
            } else {
                let diff_ms = (event.timestamp - now_ns) / 1_000_000;
                if diff_ms < 1000 {
                    format!("in {}ms", diff_ms)
                } else if diff_ms < 60_000 {
                    format!("in {:.1}s", diff_ms as f64 / 1000.0)
                } else if diff_ms < 3_600_000 {
                    format!("in {:.1}m", diff_ms as f64 / 60_000.0)
                } else {
                    format!("in {:.1}h", diff_ms as f64 / 3_600_000.0)
                }
            };

            // Truncate payload to fill remaining width
            let payload = if event.payload.chars().count() > payload_width {
                format!(
                    "{}...",
                    char_truncate(&event.payload, payload_width.saturating_sub(3))
                )
            } else {
                event.payload.clone()
            };

            let type_str = match event.recurring_ns {
                Some(ns) => {
                    let secs = ns / 1_000_000_000;
                    if secs < 60 {
                        format!("cron ({}s)", secs)
                    } else if secs < 3600 {
                        format!("cron ({}m)", secs / 60)
                    } else {
                        format!("cron ({}h)", secs / 3600)
                    }
                }
                None => "once".to_string(),
            };

            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<14}", truncate_str(&event.sender, 13)),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    format!("{:<14}", truncate_str(&event.receiver, 13)),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!("{:<16}", time_str),
                    Style::default().fg(Color::Reset),
                ),
                Span::styled(
                    format!("{:<14}", type_str),
                    Style::default().fg(if event.recurring_ns.is_some() {
                        COLOR_ACTIVE
                    } else {
                        COLOR_INACTIVE
                    }),
                ),
                Span::raw(payload),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Press Esc or 'e' to close",
        Style::default().fg(COLOR_INACTIVE),
    )));

    let block = Block::default()
        .title(" Event Queue ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(lines).block(block);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_debug_console(frame: &mut Frame, app: &App) {
    let area = centered_rect(60, 40, frame.area());

    let messages = app.ticker.latest(10);

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "Debug Console",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    if messages.is_empty() {
        lines.push(Line::from(Span::styled(
            "No messages yet",
            Style::default().fg(COLOR_INACTIVE),
        )));
    } else {
        for (i, msg) in messages.iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:>2}. ", i + 1),
                    Style::default().fg(COLOR_INACTIVE),
                ),
                Span::styled(msg.clone(), Style::default().fg(Color::Yellow)),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Press Esc or 'G' to close",
        Style::default().fg(COLOR_INACTIVE),
    )));

    let block = Block::default()
        .title(" Debug Console ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let paragraph = Paragraph::new(lines).block(block);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_settings_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(50, 30, frame.area());

    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "Settings",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    for i in 0..app.config.settings_count() {
        let Some(item) = app.config.settings_item(i) else {
            continue;
        };
        let selected = i == app.settings_selected;
        let editing = selected && app.settings_edit_buffer.is_some();
        let prefix = if selected { "▸ " } else { "  " };
        let prefix_span = Span::styled(
            prefix,
            Style::default().fg(if selected {
                Color::Cyan
            } else {
                COLOR_INACTIVE
            }),
        );
        let label_style = Style::default().fg(if selected {
            Color::Reset
        } else {
            COLOR_INACTIVE
        });

        match item {
            config::SettingItem::Toggle { label, value } => {
                let toggle = if value { "[ON] " } else { "[OFF]" };
                let toggle_color = if value { Color::Green } else { Color::Red };
                lines.push(Line::from(vec![
                    prefix_span,
                    Span::styled(
                        toggle,
                        Style::default()
                            .fg(toggle_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" {}", label), label_style),
                ]));
            }
            config::SettingItem::Text { label, value } => {
                let display: String = if editing {
                    let buf = app.settings_edit_buffer.as_deref().unwrap_or("");
                    format!("[{}_]", buf)
                } else if value.is_empty() {
                    "[unset]".to_string()
                } else {
                    format!("[{}]", value)
                };
                let value_color = if editing {
                    Color::Yellow
                } else if value.is_empty() {
                    COLOR_INACTIVE
                } else {
                    Color::Green
                };
                lines.push(Line::from(vec![
                    prefix_span,
                    Span::styled(
                        display,
                        Style::default()
                            .fg(value_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!(" {}", label), label_style),
                ]));
            }
        }
    }

    lines.push(Line::from(""));
    let hint = if app.settings_edit_buffer.is_some() {
        "Type to edit  Enter:Save  Esc:Cancel"
    } else {
        "↑↓:Select  Enter:Toggle/Edit  Esc:Close"
    };
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(COLOR_INACTIVE),
    )));

    let block = Block::default()
        .title(" Settings ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(lines).block(block);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn render_sidebar_popup(frame: &mut Frame, app: &App, panel: SidebarPanel) {
    let area = centered_rect(70, 60, frame.area());

    let (title, lines) = match panel {
        SidebarPanel::Projects => {
            let mut lines: Vec<Line> = Vec::new();
            if app.projects.is_empty() {
                lines.push(Line::from(Span::styled(
                    "No active projects.",
                    Style::default().fg(COLOR_INACTIVE),
                )));
            } else {
                for p in &app.projects {
                    lines.push(Line::from(Span::styled(
                        format!("{}. {}", p.id, p.name),
                        Style::default().fg(Color::Reset),
                    )));
                }
            }
            (" Projects ", lines)
        }
        SidebarPanel::Events => {
            // Events popup is handled by the existing show_events popup
            unreachable!()
        }
        SidebarPanel::ChainOfCommand => {
            let mut lines: Vec<Line> = Vec::new();
            if app.command_tree.is_empty() {
                lines.push(Line::from(Span::styled(
                    "No agents yet.",
                    Style::default().fg(COLOR_INACTIVE),
                )));
            } else {
                for node in &app.command_tree {
                    let (health_color, icon) = match node.health {
                        HealthState::Running => (Color::Green, "●"),
                        HealthState::Idle => (Color::Yellow, "○"),
                    };
                    let is_focus = node.session_name == app.focus_parent;

                    let name_style = if is_focus {
                        Style::default().fg(COLOR_ACTIVE)
                    } else {
                        Style::default().fg(Color::Reset)
                    };

                    let mut prefix = String::new();
                    if node.depth > 0 {
                        for d in 1..node.depth {
                            if d < node.ancestor_is_last.len() && node.ancestor_is_last[d] {
                                prefix.push_str("    ");
                            } else {
                                prefix.push_str(" │  ");
                            }
                        }
                        if node.is_last_sibling {
                            prefix.push_str(" └── ");
                        } else {
                            prefix.push_str(" ├── ");
                        }
                    }

                    let indicator = if is_focus { "► " } else { "  " };

                    lines.push(Line::from(vec![
                        Span::styled(indicator, Style::default().fg(COLOR_ACTIVE)),
                        Span::styled(prefix, Style::default().fg(COLOR_INACTIVE)),
                        Span::styled(format!("{} ", node.name), name_style),
                        Span::styled(icon, Style::default().fg(health_color)),
                    ]));
                }
            }
            (" Chain of Command ", lines)
        }
    };

    let mut all_lines = lines;
    all_lines.push(Line::from(""));
    all_lines.push(Line::from(Span::styled(
        "Press Esc or Enter to close",
        Style::default().fg(COLOR_INACTIVE),
    )));

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(COLOR_ACTIVE))
        .padding(Padding::horizontal(1));

    let paragraph = Paragraph::new(all_lines).block(block);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

/// Return a valid UTF-8 prefix of `s` containing at most `max_chars` characters.
fn char_truncate(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    if s.chars().count() > max_len {
        format!("{}…", char_truncate(s, max_len - 1))
    } else {
        s.to_string()
    }
}

fn format_countdown_ns(target_ns: u64, now_ns: u64) -> String {
    if target_ns <= now_ns {
        return "due now".to_string();
    }

    let total_secs = (target_ns - now_ns) / 1_000_000_000;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;

    if hours > 0 {
        format!("in {}:{:02}:{:02}", hours, mins, secs)
    } else {
        format!("in {:02}:{:02}", mins, secs)
    }
}

/// Create a centered rectangle
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
